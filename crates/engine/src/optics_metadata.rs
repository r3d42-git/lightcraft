//! Optional, read-only native metadata supplement for the R3D optics adapter.
//!
//! ExifTool runs as a separate local helper, never as an in-process dependency. Only
//! explicit read switches and stdin are used: it cannot update the source file.
//! No config file or user-defined ExifTool commands are loaded. Web builds retain
//! the upstream metadata reader.
use lightcraft_meta::Metadata;

/// Extract the upstream metadata and fill missing optical identity from the helper.
/// User fields (rating, GPS, caption, etc.) never come from the helper.
pub fn extract(bytes: &[u8]) -> Metadata {
    let meta = lightcraft_meta::extract(bytes);
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut meta = meta;
        if lightcraft_raw::probe(bytes).is_some() {
            match native::supplement(bytes) {
                Ok(Some(extra)) => apply(&mut meta, &extra),
                Ok(None) => {}
                Err(why) => log::warn!("R3D optical metadata: {why}"),
            }
        }
        meta
    }
    #[cfg(target_arch = "wasm32")]
    meta
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Supplement {
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length: Option<f64>,
    pub aperture: Option<f64>,
    pub focal_length_35mm: Option<f64>,
}

fn usable_name(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    !value.is_empty()
        && value.len() <= 512
        && !value.starts_with("unknown")
        && value != "n/a"
        && value != "none"
        && value.parse::<f64>().is_err()
        && !value.contains(" or ")
}

/// Parse only the explicitly requested optical tags. Ambiguous Composite LensID
/// descriptions are not usable identities, even when they contain plausible names.
pub fn parse_report(bytes: &[u8]) -> Result<Supplement, String> {
    if bytes.len() > 64 * 1024 {
        return Err("metadata reply exceeds 64 KiB".into());
    }
    let rows: Vec<serde_json::Value> = serde_json::from_slice(bytes).map_err(|e| format!("invalid metadata reply: {e}"))?;
    if rows.len() != 1 {
        return Err("expected exactly one metadata record".into());
    }
    let row = rows.first().and_then(serde_json::Value::as_object).ok_or("metadata record is not an object")?;
    if row.keys().any(|k| k.ends_with(":Error") || k == "Error") {
        return Err("helper could not read the RAW metadata".into());
    }
    let text = |tags: &[&str]| {
        tags.iter().find_map(|tag| row.get(*tag).and_then(serde_json::Value::as_str).filter(|s| usable_name(s)).map(|s| s.trim().to_string()))
    };
    let number = |tags: &[&str], max: f64| {
        tags.iter().find_map(|tag| row.get(*tag).and_then(serde_json::Value::as_f64).filter(|v| v.is_finite() && *v > 0.0 && *v < max))
    };
    Ok(Supplement {
        make: text(&["IFD0:Make", "QuickTime:Make"]),
        model: text(&["IFD0:Model", "QuickTime:Model"]),
        lens_make: text(&["ExifIFD:LensMake"]),
        lens_model: text(&["ExifIFD:LensModel", "Canon:LensModel", "Sony:LensModel", "Composite:LensID", "Panasonic:LensType"]),
        focal_length: number(&["ExifIFD:FocalLength", "Composite:FocalLength"], 10_000.0),
        aperture: number(&["ExifIFD:FNumber"], 100.0),
        focal_length_35mm: number(&["ExifIFD:FocalLengthIn35mmFormat", "Composite:FocalLength35efl"], 100_000.0),
    })
}

#[cfg(any(not(target_arch = "wasm32"), test))]
fn apply(meta: &mut Metadata, extra: &Supplement) {
    let fill = |current: &mut Option<String>, incoming: &Option<String>| {
        if current.as_deref().is_none_or(|s| !usable_name(s)) && incoming.is_some() {
            current.clone_from(incoming);
        }
    };
    fill(&mut meta.make, &extra.make);
    fill(&mut meta.model, &extra.model);
    fill(&mut meta.lens_make, &extra.lens_make);
    fill(&mut meta.lens_model, &extra.lens_model);
    meta.focal_length = meta.focal_length.or(extra.focal_length);
    meta.f_number = meta.f_number.or(extra.aperture);
    meta.focal_length_35mm = meta.focal_length_35mm.or(extra.focal_length_35mm);
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::{Supplement, parse_report};
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    type Cached = Result<Supplement, String>;
    static CACHE: OnceLock<Mutex<HashMap<String, Cached>>> = OnceLock::new();
    const MAX_INPUT: usize = 256 * 1024 * 1024;

    pub(super) fn supplement(bytes: &[u8]) -> Result<Option<Supplement>, String> {
        let Some(helper) = helper_path() else { return Ok(None) };
        if bytes.len() > MAX_INPUT {
            return Err("RAW exceeds metadata helper input limit (256 MiB); using built-in metadata".into());
        }
        let key = format!("{}:{}", helper.display(), lightcraft_preview::hash_bytes(bytes));
        // A single worker prevents a bulk import from spawning an unbounded number
        // of processes. Results are reused by import, previews and exports.
        let mut cache = CACHE.get_or_init(|| Mutex::new(HashMap::new())).lock().map_err(|_| "metadata cache unavailable")?;
        if let Some(result) = cache.get(&key) {
            return result.clone().map(Some);
        }
        let result = read(&helper, bytes);
        if cache.len() >= 2048 {
            cache.clear();
        }
        cache.insert(key, result.clone());
        result.map(Some)
    }

    fn helper_path() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("LIGHTCRAFT_R3D_EXIFTOOL") {
            return (!path.is_empty()).then(|| PathBuf::from(path));
        }
        crate::config::config_dir().map(|p| p.join("optics/exiftool/exiftool")).filter(|p| p.is_file())
    }

    fn read(helper: &std::path::Path, bytes: &[u8]) -> Cached {
        let mut child = Command::new(helper)
            .args([
                "-config",
                "",
                "-j",
                "-G1",
                "-s",
                "-Make",
                "-Model",
                "-LensMake",
                "-LensModel",
                "-LensID",
                "-LensType",
                "-FocalLength#",
                "-FNumber#",
                "-FocalLengthIn35mmFormat#",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start metadata helper: {e}"))?;
        let Some(mut input) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("metadata helper stdin unavailable".into());
        };
        let Some(output) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("metadata helper stdout unavailable".into());
        };
        std::thread::scope(|scope| {
            let writer = scope.spawn(move || input.write_all(bytes));
            let reader = scope.spawn(move || {
                let mut reply = Vec::new();
                output.take(64 * 1024 + 1).read_to_end(&mut reply).map(|_| reply)
            });
            let deadline = Instant::now() + Duration::from_secs(15);
            let outcome = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break if status.success() { Ok(()) } else { Err("metadata helper failed".to_string()) },
                    Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                    Ok(None) => break Err("metadata helper timed out after 15 seconds".into()),
                    Err(e) => break Err(format!("metadata helper wait failed: {e}")),
                }
            };
            if outcome.is_err() {
                let _ = child.kill();
                let _ = child.wait();
            }
            let sent = writer.join().map_err(|_| "metadata input worker stopped".to_string())?;
            let reply = reader.join().map_err(|_| "metadata output worker stopped".to_string())?.map_err(|e| e.to_string())?;
            outcome?;
            sent.map_err(|e| format!("cannot send RAW to metadata helper: {e}"))?;
            parse_report(&reply)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_maker_note_identity_without_replacing_existing_metadata() {
        let extra = parse_report(br#"[{"IFD0:Make":"SONY","Composite:LensID":"Sony FE 35mm F1.8","ExifIFD:FocalLength":35}]"#).unwrap();
        let mut meta = Metadata { lens_model: Some("Unknown (32767)".into()), artist: Some("Keep author".into()), ..Default::default() };
        apply(&mut meta, &extra);
        assert_eq!(meta.lens_model.as_deref(), Some("Sony FE 35mm F1.8"));
        assert_eq!(meta.artist.as_deref(), Some("Keep author"));
        meta.lens_model = Some("Explicit EXIF identity".into());
        apply(&mut meta, &extra);
        assert_eq!(meta.lens_model.as_deref(), Some("Explicit EXIF identity"));
    }

    #[test]
    fn unknown_numeric_and_ambiguous_lenses_are_not_identities() {
        for name in ["Unknown 8-36mm", "n/a", "65535", "Canon EF 50mm or Sigma 50mm", ""] {
            let value = serde_json::json!([{"Composite:LensID": name}]);
            assert_eq!(parse_report(&serde_json::to_vec(&value).unwrap()).unwrap().lens_model, None);
        }
    }

    #[test]
    fn rejects_invalid_records_and_never_uses_user_fields() {
        for bytes in [&b"[]"[..], &b"[{},{}]"[..], &b"broken"[..], &b"[{\"ExifTool:Error\":\"bad raw\"}]"[..]] {
            assert!(parse_report(bytes).is_err());
        }
        let extra = parse_report(br#"[{"ExifIFD:FocalLength":-1,"ExifIFD:FNumber":0,"XMP:Rating":5}]"#).unwrap();
        assert_eq!(extra, Supplement::default());
    }
}
