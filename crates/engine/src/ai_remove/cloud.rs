//! Cloud reconstruction keeps local geometry, authorization and raster persistence.
//! Shadow coverage is determined by the existing trusted ObjectClear stage; only
//! original RGB and the final authorized mask are sent to ChatGPT through Codex.
use super::{AiRemove, Job, Phase, SelectionMask, codex_client, error, input, patch, protocol, runner};
use crate::{Result, Session};
use lightcraft_codecs::{DecodeOptions, EncodeImage, EncodeMeta};
use lightcraft_develop::Spot;
use lightcraft_raster::Rgba8;
use runner::{RetouchIdentity, RetouchInput, RetouchOutput, RetouchProvenance, RetouchRequest, RetouchRunner};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};

impl Session {
    pub(super) fn ai_remove_start_cloud(&mut self, include_effects: Option<bool>) -> Result<Value> {
        self.ai_remove_poll();
        if self.ai_remove.busy() {
            return Err(error("AI Remove is already running."));
        }
        if self.ai_remove.selecting {
            return Err(error("Wait for the object selection."));
        }
        if self.ai_models.busy() {
            return Err(error("Wait for model management to finish."));
        }
        let segmented = self.ai_remove.object_selection.as_ref().and_then(|o| o.seg.clone());
        if self.ai_remove.object_selection.is_some() && segmented.is_none() {
            return Err(error("Select an object first."));
        }
        if let Some(seg) = &segmented
            && SelectionMask::new(seg).map_err(error)?.is_empty()
        {
            return Err(error("The selected mask is empty."));
        }
        let id = self.active().ok_or_else(|| error("Select a photo."))?;
        let d = self.develop_of(id).unwrap_or_default();
        let (spot, index) = if segmented.is_some() {
            (Spot::default(), None)
        } else if let Some(selection) = self.ai_remove.selection.clone() {
            (selection, None)
        } else {
            let index = self.active_spot.ok_or_else(|| error("Paint an AI selection first."))?;
            (d.spots.get(index).cloned().ok_or_else(|| error("Select a spot first."))?, Some(index))
        };
        if d.ai_edits.len() >= 8 {
            return Err(error("This photo already has eight AI patches."));
        }
        let executable = codex_client::find_executable().map_err(error)?;
        let requested_effects = include_effects.or(self.ai_remove.include_effects).unwrap_or(true);
        // No local model is needed for object-only cloud reconstruction.
        let effects_package = if requested_effects {
            let resolved = crate::ai_models::root()
                .and_then(|root| crate::ai_models::resolve(&root, Some(crate::ai_models::EFFECTS_MODEL_ID), Some("objectclear"), false));
            match resolved {
                Ok(package) if package.supports_effects() => Some(package),
                _ if include_effects == Some(true) => return Err(error("Cloud shadow removal needs local ObjectClear shadow detection.")),
                _ => None,
            }
        } else {
            None
        };
        let effects = effects_package.is_some();
        let refinements = self.ai_remove.object_selection.as_ref().map(|o| o.refinements.clone()).unwrap_or_default();
        let photo = self.catalog.photo(id).cloned().ok_or_else(|| error("Photo missing."))?;
        let source = self.media.source_ref(&photo, crate::media::SourceLevel::Full);
        let info = self.source_info(id);
        let settings = (*d).clone();
        let job = Job::spawn(&self.activity, "ChatGPT via Codex", move |cancel, progress| {
            let deadline = Instant::now() + Duration::from_secs(1200);
            progress(Phase::Preparing);
            let mut prepared = match segmented {
                Some(seg) => input::prepare_segmented_protected(source, info, settings, seg, &refinements, 0, cancel)?,
                None => input::prepare(source, info, settings, spot, index.is_none(), 0, cancel)?,
            };
            prepared.request.effects = effects;
            let cloud = CloudRunner { executable, deadline };
            if let Some(package) = effects_package {
                let root = crate::ai_models::root().map_err(|e| e.to_string())?;
                let coverage = {
                    let _store_lock = crate::ai_models::store_lock(&root).map_err(|e| e.to_string())?;
                    package.verify_execution().map_err(|e| e.to_string())?;
                    progress(Phase::Effects);
                    let detector = protocol::ProtocolRunner {
                        runtime: &package.runtime,
                        backend: &package.backend,
                        package: Some(&package),
                        deadline: Some(deadline),
                    };
                    runner::invoke_checked(&prepared.request, &detector, cancel)?.coverage.ok_or("Shadow detection returned no coverage.")?
                };
                execute_with_coverage(prepared, coverage, &cloud, cancel, progress)
            } else {
                runner::execute_with_progress(prepared, &cloud, cancel, progress)
            }
        })
        .map_err(error)?;
        self.ai_remove = AiRemove {
            use_cloud: true,
            include_effects: self.ai_remove.include_effects,
            reconstruct: self.ai_remove.reconstruct,
            effects_active: effects,
            reconstruction_active: false,
            object_selection: self.ai_remove.object_selection.clone(),
            selecting: false,
            selection_revision: self.ai_remove.selection_revision,
            phase: Phase::Loading,
            cancel_reason: None,
            message: "Sending selected region to ChatGPT through Codex…".into(),
            draft: None,
            preview_settings: None,
            preview_original: false,
            tag: self.ai_remove_tag(),
            spot: index,
            selection: self.ai_remove.selection.clone(),
            selection_tag: self.ai_remove.selection_tag.clone(),
            job: Some(job),
        };
        Ok(json!({"running":true,"provider":"chatgpt","effectsActive":effects}))
    }
}

fn execute_with_coverage(
    prepared: input::PreparedInput,
    coverage: Vec<u8>,
    cloud: &dyn RetouchRunner,
    cancel: &AtomicBool,
    progress: &dyn Fn(Phase),
) -> std::result::Result<lightcraft_develop::AiEdit, String> {
    let mut reconstruction = prepared.request.clone();
    if coverage.len() != reconstruction.mask.len()
        || !coverage.iter().zip(&reconstruction.mask).all(|(c, m)| (*c == 0 || *c == 255) && (*m == 0 || *c == 255))
    {
        return Err("Invalid shadow coverage.".into());
    }
    reconstruction.mask = coverage.iter().zip(&reconstruction.protected).map(|(v, p)| if *p { 0 } else { *v }).collect();
    reconstruction.effects = false;
    if !reconstruction.mask.contains(&255) {
        return Err("No unprotected pixels to reconstruct.".into());
    }
    progress(Phase::Reconstruction);
    // The local detector's RGB is intentionally never used as cloud input.
    let mut output = runner::invoke_checked(&reconstruction, cloud, cancel)?;
    output.coverage = Some(coverage);
    output.provenance.model = "ObjectClear coverage + ChatGPT image generation".into();
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    progress(Phase::Finalizing);
    patch::construct(prepared, output)
}

struct CloudRunner {
    executable: PathBuf,
    deadline: Instant,
}
impl RetouchRunner for CloudRunner {
    fn identity(&self) -> RetouchIdentity {
        RetouchIdentity { model: "chatgpt-image-generation".into(), runtime: self.executable.clone(), adapter: "codex-app-server" }
    }
    fn run(&self, request: RetouchRequest<'_>, cancel: &AtomicBool) -> std::result::Result<RetouchOutput, String> {
        if request.input.effects {
            return Err("Cloud reconstruction requires resolved local shadow coverage.".into());
        }
        let canvas = Canvas::new(request.input)?;
        let scratch = Scratch::new()?;
        let image = scratch.0.join("photo.png");
        let mask = scratch.0.join("edit-mask.png");
        std::fs::write(&image, png(&canvas.image)?).map_err(|e| e.to_string())?;
        std::fs::write(&mask, png(&canvas.mask)?).map_err(|e| e.to_string())?;
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled.".into());
        }
        let prompt = "Edit image 1 (the photo) with built-in image generation exactly once. Image 2 is an edit mask: WHITE pixels identify the object and any associated shadow to remove, BLACK pixels must remain unchanged. Remove the indicated object and reconstruct natural background only in the white region. Keep the exact square canvas, framing, alignment, lighting, color and every other object. Black padding is intentional; do not crop, zoom or move anything. Do not draw the mask into the result. Return one opaque edited photo, no text. Use the supplied photo as the edit target. Do not use any API-key fallback or other tools.";
        let generated = codex_client::edit(&self.executable, &[image, mask], prompt, cancel, self.deadline)?;
        let rgb = canvas.decode(&generated.png, request.input)?;
        Ok(RetouchOutput {
            width: request.input.width,
            height: request.input.height,
            rgb,
            coverage: None,
            provenance: RetouchProvenance {
                model: "ChatGPT image generation".into(),
                revision: generated.provenance,
                identity: request.identity.clone(),
            },
        })
    }
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> std::result::Result<Self, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("lightcraft-cloud-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path).map_err(|e| e.to_string())?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn png(image: &Rgba8) -> std::result::Result<Vec<u8>, String> {
    lightcraft_codecs::encode_png(&EncodeImage::rgba8(image), &EncodeMeta::default()).map_err(|e| e.to_string())
}

/// A square envelope avoids model output aspect-ratio changes stretching the patch.
/// Only image pixels are serialized: no source filename, EXIF, GPS or catalog data.
struct Canvas {
    image: Rgba8,
    mask: Rgba8,
    x: usize,
    y: usize,
    gain: f32,
}
impl Canvas {
    fn new(input: &RetouchInput) -> std::result::Result<Self, String> {
        let count = input.width.checked_mul(input.height).filter(|n| *n > 0).ok_or("Invalid cloud input dimensions.")?;
        if input.width > 1024 || input.height > 1024 || input.rgb.len() != count || input.mask.len() != count || input.protected.len() != count {
            return Err("Invalid cloud input pixels or mask.".into());
        }
        let matrix = lightcraft_color::REC2020.to_space(&lightcraft_color::SRGB);
        let colors: Vec<[f32; 3]> = input.rgb.iter().map(|p| matrix.apply_f32(*p)).collect();
        if !colors.iter().flatten().all(|v| v.is_finite()) {
            return Err("Non-finite cloud input.".into());
        }
        // One reversible exposure scale prevents HDR clipping. Gamut conversion is
        // clipped only for the inference preview, never for unselected original pixels.
        let gain = colors.iter().flatten().copied().fold(1.0f32, f32::max);
        let side = input.width.max(input.height);
        let (x, y) = ((side - input.width) / 2, (side - input.height) / 2);
        let mut image = Rgba8::new(side, side);
        let mut mask = Rgba8::new(side, side);
        image.data.fill([0, 0, 0, 255]);
        mask.data.fill([0, 0, 0, 255]);
        for (i, color) in colors.into_iter().enumerate() {
            let at = (y + i / input.width) * side + x + i % input.width;
            let encode = |v| lightcraft_color::transfer::encode_srgb8(v / gain);
            if let Some(pixel) = image.data.get_mut(at) {
                *pixel = [encode(color[0]), encode(color[1]), encode(color[2]), 255];
            }
            if input.mask.get(i) == Some(&255)
                && input.protected.get(i) == Some(&false)
                && let Some(pixel) = mask.data.get_mut(at)
            {
                *pixel = [255; 4];
            }
        }
        Ok(Self { image, mask, x, y, gain })
    }
    fn decode(&self, bytes: &[u8], input: &RetouchInput) -> std::result::Result<Vec<[f32; 3]>, String> {
        if bytes.len() > 32 * 1024 * 1024 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err("Cloud result must be a bounded PNG image.".into());
        }
        let decoded = lightcraft_codecs::decode(bytes, DecodeOptions { max_size: None, max_pixels: 16 * 1024 * 1024 }).map_err(|e| e.to_string())?;
        if decoded.image.width != decoded.image.height || decoded.image.width < 64 || decoded.orientation != 1 {
            return Err("ChatGPT changed the image canvas. Try again; no retouch was applied.".into());
        }
        if decoded.alpha.as_ref().is_some_and(|a| a.data.iter().any(|v| !v.is_finite() || *v < 0.999)) {
            return Err("ChatGPT returned a transparent result.".into());
        }
        let working = decoded.to_working();
        let scale = working.width as f32 / self.image.width as f32;
        let mut rgb = input.rgb.clone();
        for (i, pixel) in rgb.iter_mut().enumerate() {
            if input.mask.get(i) != Some(&255) || input.protected.get(i) != Some(&false) {
                continue;
            }
            let x = ((self.x + i % input.width) as f32 + 0.5) * scale;
            let y = ((self.y + i / input.width) as f32 + 0.5) * scale;
            *pixel = working.sample_bilinear(x, y).map(|v| v * self.gain);
        }
        if !rgb.iter().flatten().all(|v| v.is_finite()) {
            return Err("ChatGPT returned non-finite pixel data.".into());
        }
        Ok(rgb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> RetouchInput {
        RetouchInput {
            width: 128,
            height: 64,
            rgb: vec![[0.2, 0.2, 0.2]; 128 * 64],
            mask: vec![255; 128 * 64],
            protected: vec![false; 128 * 64],
            rect: [0.0, 0.0, 1.0, 1.0],
            seed: 0,
            steps: 20,
            effects: false,
        }
    }
    #[test]
    fn cloud_roundtrip_preserves_unselected_hdr_and_protected_pixels_exactly() {
        let mut input = input();
        input.rgb[0] = [4.0, 2.0, 1.0];
        input.mask[0] = 0;
        input.rgb[1] = [0.9, 0.4, 0.1];
        input.protected[1] = true;
        let canvas = Canvas::new(&input).unwrap();
        assert_eq!(canvas.mask.data[canvas.y * 128 + 1], [0, 0, 0, 255]);
        let result = canvas.decode(&png(&canvas.image).unwrap(), &input).unwrap();
        assert_eq!(result[0], input.rgb[0]);
        assert_eq!(result[1], input.rgb[1]);
        for v in result[2] {
            assert!((v - 0.2).abs() < 0.012, "{v}");
        }
    }
    #[test]
    fn cloud_rejects_changed_aspect_transparency_and_invalid_input() {
        let mut input = input();
        let canvas = Canvas::new(&input).unwrap();
        assert!(canvas.decode(&png(&Rgba8::new(128, 64)).unwrap(), &input).is_err());
        assert!(canvas.decode(&png(&Rgba8::new(128, 128)).unwrap(), &input).is_err());
        input.rgb[0][0] = f32::NAN;
        assert!(Canvas::new(&input).is_err());
    }

    #[test]
    fn provider_switch_discards_draft_preserves_selection_and_never_starts_upload() {
        let mut s = Session::with_demo();
        super::super::tests::draft(&mut s);
        let selection = Spot::default();
        s.ai_remove.selection = Some(selection.clone());
        let before = s.develop_of(s.active().unwrap()).unwrap();
        s.execute("aiRemove.provider", &json!({"provider":"chatgpt"})).unwrap();
        assert!(s.ai_remove.use_cloud);
        assert!(!s.ai_remove.busy());
        assert!(s.ai_remove.draft.is_none());
        assert_eq!(s.ai_remove.selection.as_ref().unwrap().points, selection.points);
        assert_eq!(*s.develop_of(s.active().unwrap()).unwrap(), *before);
        assert_eq!(s.execute("aiRemove.status", &json!({})).unwrap()["provider"], "chatgpt");
        assert!(s.execute("aiRemove.provider", &json!({"provider":"unknown"})).is_err());
        assert!(s.execute("aiRemove.provider", &json!({})).is_err());
        assert!(s.ai_remove.use_cloud);
        s.ai_remove.selecting = true;
        assert!(s.execute("aiRemove.provider", &json!({"provider":"local"})).is_err());
        assert!(s.ai_remove.use_cloud);
    }

    #[test]
    fn cloud_effects_reconstruct_original_rgb_and_keep_protected_pixels() {
        struct Fake;
        impl RetouchRunner for Fake {
            fn identity(&self) -> RetouchIdentity {
                RetouchIdentity { model: "fake".into(), runtime: "fake".into(), adapter: "test" }
            }
            fn run(&self, r: RetouchRequest<'_>, _: &AtomicBool) -> std::result::Result<RetouchOutput, String> {
                assert_eq!(r.input.rgb, vec![[0.2, 0.3, 0.4]; 4]);
                assert_eq!(r.input.mask, vec![0, 255, 255, 0]);
                assert!(!r.input.effects);
                Ok(RetouchOutput {
                    width: 4,
                    height: 1,
                    rgb: vec![[0.7, 0.6, 0.5]; 4],
                    coverage: None,
                    provenance: RetouchProvenance { model: "fake".into(), revision: "1".into(), identity: r.identity.clone() },
                })
            }
        }
        let prepared = || input::PreparedInput {
            support: None,
            request: RetouchInput {
                width: 4,
                height: 1,
                rgb: vec![[0.2, 0.3, 0.4]; 4],
                mask: vec![0, 255, 0, 0],
                protected: vec![true, false, false, false],
                rect: [0.0, 0.0, 1.0, 1.0],
                seed: 0,
                steps: 20,
                effects: true,
            },
        };
        let edit = execute_with_coverage(prepared(), vec![255, 255, 255, 0], &Fake, &AtomicBool::new(false), &|_| {}).unwrap();
        let pixels = edit.decode_pixels().unwrap();
        assert_eq!(pixels[0], [0.2, 0.3, 0.4, 0.0]);
        assert_eq!(pixels[1], [0.7, 0.6, 0.5, 1.0]);
        assert_eq!(pixels[2], [0.7, 0.6, 0.5, 1.0]);
        assert_eq!(pixels[3], [0.2, 0.3, 0.4, 0.0]);
        assert!(execute_with_coverage(prepared(), vec![255, 0, 255, 0], &Fake, &AtomicBool::new(false), &|_| {}).is_err());
        assert!(execute_with_coverage(prepared(), vec![255, 255, 255, 0], &Fake, &AtomicBool::new(true), &|_| {}).is_err());
    }

    #[test]
    #[ignore = "explicit live ChatGPT subscription test; consumes included usage"]
    fn live_chatgpt_masked_edit() {
        assert_eq!(std::env::var("LIGHTCRAFT_CLOUD_LIVE").as_deref(), Ok("1"));
        let mut input = input();
        input.mask.fill(0);
        let red = lightcraft_color::SRGB.to_space(&lightcraft_color::REC2020).apply_f32([0.8, 0.0, 0.0]);
        for y in 20..44 {
            for x in 48..80 {
                let index = y * input.width + x;
                input.rgb[index] = red;
                input.mask[index] = 255;
            }
        }
        let runner = CloudRunner { executable: codex_client::find_executable().unwrap(), deadline: Instant::now() + Duration::from_secs(600) };
        let result = runner::invoke_checked(&input, &runner, &AtomicBool::new(false)).unwrap();
        assert_eq!(result.rgb.len(), input.rgb.len());
        for (i, pixel) in result.rgb.iter().enumerate() {
            if input.mask[i] == 0 {
                assert_eq!(pixel, &input.rgb[i]);
            }
        }
        let center = result.rgb[32 * input.width + 64];
        assert!((center[0] - center[1]).abs() < 0.12, "object removal failed: {center:?}");
        println!("Cloud masked edit completed; protected/outside pixels identical; provenance: {}", result.provenance.revision);
    }
}
