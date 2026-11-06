//! Lens profile corrections from a lens database ([`lightcraft_lensdb`]).
//!
//! When a raw carries no lens correction of its own (DNG opcodes, or a camera correction a raw reader converts,
//! [`crate::files::embedded_lens`]), the lens named in its EXIF is looked up in a lensfun-format database. The
//! database is lensfun's `data/db` (CC BY-SA 3.0): never bundled with the app, it is downloaded at run time into the
//! app's data folder ([`crate::lensfun_download`], which the desktop app offers on its first launch and Settings can start), or it is the
//! directory in `LIGHTCRAFT_LENSFUN_DIR` (which wins). The profile is converted to the same
//! [`lightcraft_develop::EmbeddedLens`] form (marked [`LensSource::Database`]), so "Enable Profile Corrections", its
//! amount sliders, the GPU path and DNG export treat both alike. Without a database nothing changes.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use lightcraft_develop::{EmbeddedLens, LensSource};
use lightcraft_lensdb::{LensDb, Shot};

/// The environment variable naming the lens database directory.
pub const ENV: &str = "LIGHTCRAFT_LENSFUN_DIR";

/// The database folder a host set up ([`use_dir`]), read when `LIGHTCRAFT_LENSFUN_DIR` is not set.
static HOST_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);
/// The loaded database and the folder it was read from (`None` inside: nothing usable there).
static LOADED: RwLock<Option<(PathBuf, Option<Arc<LensDb>>, u64)>> = RwLock::new(None);
/// Bumped whenever the database in use changes ([`reload`]): photos looked up before may now find a profile.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Bumped when the database's files were replaced ([`replaced`]): profiles found before may now be different.
static REPLACED: AtomicU64 = AtomicU64::new(0);

/// Use `dir` (the app's downloaded lensfun data, [`crate::lensfun_download::default_dir`]) when
/// `LIGHTCRAFT_LENSFUN_DIR` is not set. Hosts with a file system call it; tests and sessions that don't stay
/// without a database.
pub fn use_dir(dir: Option<PathBuf>) {
    *HOST_DIR.write().unwrap_or_else(PoisonError::into_inner) = dir;
    reload();
}

/// The database's files were replaced (a download put a new database in place): read it again, and photos whose
/// profile came from the old one are looked up again too ([`replacements`]).
pub fn replaced() {
    REPLACED.fetch_add(1, Ordering::Relaxed);
    reload();
}

/// Changes whenever the database's files were replaced ([`replaced`]); selecting a folder ([`use_dir`]) or reading
/// it again doesn't change it, so photos with a profile are not read again at every start.
pub fn replacements() -> u64 {
    REPLACED.load(Ordering::Relaxed)
}

/// The folder the host selected ([`use_dir`]), whatever `LIGHTCRAFT_LENSFUN_DIR` says.
pub fn host_dir() -> Option<PathBuf> {
    HOST_DIR.read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Read the database again (after a download or an update): the next lookup sees the new data.
pub fn reload() {
    // under the cache lock, so a load of the old files that finishes meanwhile can't be published ([`database`])
    let mut loaded = LOADED.write().unwrap_or_else(PoisonError::into_inner);
    *loaded = None;
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Changes whenever the database in use may have changed.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// The folder the database is read from: `LIGHTCRAFT_LENSFUN_DIR`, else the host's ([`use_dir`]).
pub fn dir() -> Option<PathBuf> {
    source().map(|(d, _)| d)
}

/// [`dir`], and whether it is the app's own downloaded folder (read only when whole, [`crate::lensfun_download::complete`])
/// rather than the user's.
fn source() -> Option<(PathBuf, bool)> {
    match std::env::var_os(ENV).filter(|v| !v.is_empty()) {
        Some(d) => Some((PathBuf::from(d), false)),
        None => crate::config::config_dir()
            .map(|d| d.join("optics/lensfun"))
            .filter(|d| d.is_dir())
            .map(|d| (d, false))
            .or_else(|| HOST_DIR.read().unwrap_or_else(PoisonError::into_inner).clone().map(|d| (d, true))),
    }
}

/// The database in use, read once per folder (and again after [`reload`]); `None` when none is configured, the
/// folder is missing or it holds no lenses.
pub fn database() -> Option<Arc<LensDb>> {
    let (dir, downloaded) = source()?;
    if let Some((d, db, _)) = LOADED.read().unwrap_or_else(PoisonError::into_inner).as_ref()
        && *d == dir
    {
        return db.clone();
    }
    let generation = generation();
    // the app's own folder without its marker is a download that never finished: not read (the start fetches it)
    let db = if downloaded && !crate::lensfun_download::complete(&dir) {
        // info, not a warning: before the first download this is every user's state (logged once per reload)
        log::info!("lens database: no finished download in {} yet; lens profiles are not applied", dir.display());
        None
    } else {
        load(&dir, downloaded)
    };
    let mut loaded = LOADED.write().unwrap_or_else(PoisonError::into_inner);
    // a reload while the files were read: they may be the old ones, so they aren't kept (the next call reads again)
    if GENERATION.load(Ordering::Relaxed) == generation {
        let fingerprint = db.as_deref().map_or(0, snapshot_key);
        *loaded = Some((dir, db.clone(), fingerprint));
    }
    db
}

/// Stable identity of all parsed database semantics. Computed once per reload, not
/// once per photo/frame. Legacy R3D decodes use the current database, so their
/// persistent cache keys must distinguish it across both reloads and restarts.
pub fn cache_key() -> u64 {
    let Some(db) = database() else { return 0 };
    let key = LOADED
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .and_then(|(_, loaded, key)| loaded.as_ref().filter(|loaded| Arc::ptr_eq(loaded, &db)).map(|_| *key));
    // A concurrent reload can leave this valid Arc unpublished. It still must
    // not masquerade as 'no database' (zero); compute that rare snapshot directly.
    key.unwrap_or_else(|| snapshot_key(&db))
}

fn snapshot_key(db: &LensDb) -> u64 {
    let h = lightcraft_preview::Hasher128::new().str("lens-db-snapshot-v1").str(&format!("{db:?}")).finish().0;
    (h as u64) ^ ((h >> 64) as u64) | 1
}

fn load(dir: &Path, downloaded: bool) -> Option<Arc<LensDb>> {
    let whose = if downloaded { "downloaded" } else { ENV };
    if !dir.is_dir() {
        log::warn!("lens database: {} ({whose}) is missing or not a folder", dir.display());
        return None;
    }
    match LensDb::load_dir(dir) {
        Ok((db, errors)) => {
            for (file, e) in errors {
                log::warn!("lens database: skipped {file}: {e}");
            }
            let (level, line) = summary(dir, whose, &db);
            log::log!(level, "{line}");
            (!db.lenses.is_empty()).then(|| Arc::new(db))
        }
        Err(e) => {
            log::warn!("{e}");
            None
        }
    }
}

/// The log line for a database read from `dir`: where it is and how many lenses and cameras it has, so a user
/// setting up `LIGHTCRAFT_LENSFUN_DIR` can see whether it was found; a folder without lenses is a warning.
fn summary(dir: &Path, whose: &str, db: &LensDb) -> (log::Level, String) {
    let (lenses, cameras) = (db.lenses.len(), db.cameras.len());
    if lenses == 0 {
        (
            log::Level::Warn,
            format!("lens database: no lenses in {} ({whose}): lenses 0, cameras {cameras}; lens profiles are not applied", dir.display()),
        )
    } else {
        (log::Level::Info, format!("lens database: read {} ({whose}): lenses {lenses}, cameras {cameras}", dir.display()))
    }
}

/// The database's name for the lens a photo's file calls `lens` (taken with `camera`, the make and model as the
/// library shows them): "Canon EF 70-200mm f/2.8L IS II USM". `None` without a database or a match.
pub fn profile_name(db: &LensDb, camera: &str, lens: &str) -> Option<String> {
    let cam = camera.split_once(' ').and_then(|(make, model)| db.find_camera(make, model).or_else(|| db.find_camera(make, camera)));
    let p = db.find_lens(None, lens, cam)?;
    let model = p.models.first()?;
    Some(if model.to_lowercase().starts_with(&p.maker.to_lowercase()) || p.maker.is_empty() { model.clone() } else { format!("{} {model}", p.maker) })
}

/// External profiles live outside the binary. An explicit environment path takes precedence.
pub fn database_dir() -> Option<PathBuf> {
    dir()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentStatus {
    Applied,
    Missing,
    MissingMetadata,
    UnsupportedProjection,
    RejectedConversion,
}

/// Exact component outcomes for the converted external profile. Distortion/TCA share one warp:
/// if their combined conversion fails, both supplied components are reported rejected.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ProfileReport {
    pub provider: &'static str,
    pub resolution: r3d_optics::Resolution,
    pub profile_maker: Option<String>,
    pub profile_names: Vec<String>,
    pub distortion: ComponentStatus,
    pub chromatic_aberration: ComponentStatus,
    pub vignetting: ComponentStatus,
    pub aperture_known: bool,
    pub frame_crop_factor: Option<f64>,
    #[serde(skip)]
    pub correction: Option<EmbeddedLens>,
}

/// Inspect external-profile selection and conversion without changing a photo or its settings.
/// No embedded/external components are mixed: the raw adapter retains embedded precedence.
pub fn inspect(db: &LensDb, m: &lightcraft_meta::Metadata, w: f64, h: f64, orientation: lightcraft_geom::Orientation) -> ProfileReport {
    let cameras = db
        .cameras
        .iter()
        .map(|camera| r3d_optics::CameraDescriptor {
            maker: camera.maker.clone(),
            model: camera.model.clone(),
            mount: camera.mount.clone(),
            compatible_mounts: db
                .mounts
                .iter()
                .filter(|mount| mount.name.eq_ignore_ascii_case(&camera.mount))
                .flat_map(|mount| mount.compatible_mounts.iter().cloned())
                .collect(),
            crop_factor: camera.crop_factor,
            // Lensfun documents lowercase initial mounts as reserved for fixed-lens cameras:
            // https://lensfun.github.io/manual/latest/structlfMount.html
            fixed_lens: camera.mount.chars().next().is_some_and(char::is_lowercase),
        })
        .collect::<Vec<_>>();
    // A maker-less specification is only an identity when a profile explicitly names it.
    // Keep upstream's Nikon alias fallback without weakening conservative ambiguity checks.
    let spec_name = m.lens_model.is_none().then(|| m.lens_spec.map(lightcraft_lensdb::spec_name)).flatten();
    let profiles = db
        .lenses
        .iter()
        .map(|profile| r3d_optics::ProfileDescriptor {
            maker: profile.maker.clone(),
            names: if spec_name.as_deref().is_some_and(|name| !profile.names_exactly(name)) { Vec::new() } else { profile.models.clone() },
            mounts: profile.mounts.clone(),
            calibration_crop_factor: profile.crop_factor,
        })
        .collect::<Vec<_>>();
    let resolution = r3d_optics::resolve(
        r3d_optics::Request {
            camera_make: m.make.as_deref(),
            camera_model: m.model.as_deref(),
            lens_make: m.lens_make.as_deref(),
            lens_model: m.lens_model.as_deref().or(spec_name.as_deref()),
            frame_crop_factor: recorded_crop_factor(m.focal_length.unwrap_or(0.0), m.focal_length_35mm),
        },
        &cameras,
        &profiles,
    );
    let mut report = ProfileReport {
        provider: "lensfun_xml",
        frame_crop_factor: resolution.frame_crop_factor,
        resolution,
        profile_maker: None,
        profile_names: Vec::new(),
        distortion: ComponentStatus::Missing,
        chromatic_aberration: ComponentStatus::Missing,
        vignetting: ComponentStatus::Missing,
        aperture_known: m.f_number.is_some_and(|n| n.is_finite() && n > 0.0),
        correction: None,
    };
    let Some(profile) = report.resolution.profile_index.and_then(|index| db.lenses.get(index)) else { return report };
    report.profile_maker = Some(profile.maker.clone());
    report.profile_names = profile.models.clone();
    let Some(focal) = m.focal_length.filter(|f| f.is_finite() && *f > 0.0) else {
        report.distortion = if !profile.rectilinear {
            ComponentStatus::UnsupportedProjection
        } else if profile.distortion.is_empty() {
            ComponentStatus::Missing
        } else {
            ComponentStatus::MissingMetadata
        };
        report.chromatic_aberration = if profile.tca.is_empty() { ComponentStatus::Missing } else { ComponentStatus::MissingMetadata };
        report.vignetting = if profile.vignetting.is_empty() { ComponentStatus::Missing } else { ComponentStatus::MissingMetadata };
        return report;
    };
    let crop = report.resolution.frame_crop_factor;
    let shot = Shot { focal, aperture: m.f_number.filter(|n| n.is_finite() && *n > 0.0), distance: None, crop_factor: crop, width: w, height: h };
    // Without an aperture, choosing the widest calibrated value can brighten the
    // corners far too much. Keep geometric correction, but report shading missing.
    let mut supported_profile = profile.clone();
    if !report.aperture_known {
        supported_profile.vignetting.clear();
    }
    let correction = supported_profile.correction(&shot);
    let warp = correction.as_ref().is_some_and(|correction| correction.warp.is_some());
    let component = |supplied: bool, applied: bool| {
        if !supplied {
            ComponentStatus::Missing
        } else if applied {
            ComponentStatus::Applied
        } else {
            ComponentStatus::RejectedConversion
        }
    };
    report.distortion = if !profile.rectilinear { ComponentStatus::UnsupportedProjection } else { component(!profile.distortion.is_empty(), warp) };
    report.chromatic_aberration = component(!profile.tca.is_empty(), warp);
    report.vignetting = if !profile.vignetting.is_empty() && !report.aperture_known {
        ComponentStatus::MissingMetadata
    } else {
        component(!profile.vignetting.is_empty(), correction.as_ref().is_some_and(|correction| correction.vignette.is_some()))
    };
    report.correction = correction.map(|correction| EmbeddedLens {
        source: LensSource::Database,
        ..lightcraft_pipeline::optics::reorient_lens(&correction, orientation, w, h)
    });
    report
}

/// The database profile for a photo with metadata `m`, for its `w × h` (unoriented, default-cropped) image,
/// re-expressed after `orientation`. Fixed-lens cameras may resolve through their provider mount without a lens name.
/// `None` without a focal length or unique compatible profile.
pub fn lookup(db: &LensDb, m: &lightcraft_meta::Metadata, w: f64, h: f64, orientation: lightcraft_geom::Orientation) -> Option<EmbeddedLens> {
    inspect(db, m, w, h, orientation).correction
}

/// The EXIF-implied crop; body rounding and crop-mode policy belong to the resolver.
fn recorded_crop_factor(focal: f64, focal_35mm: Option<f64>) -> Option<f64> {
    if !focal.is_finite() || focal <= 0.0 {
        return None;
    }
    focal_35mm.map(|f35| f35 / focal).filter(|crop| crop.is_finite() && (0.1..=20.0).contains(crop))
}

/// [`crate::files::embedded_lens`], else the database profile ([`lookup`]) for a raw with metadata `meta`.
pub fn for_raw(raw: &lightcraft_raw::RawInfo, meta: impl FnOnce() -> lightcraft_meta::Metadata) -> Option<EmbeddedLens> {
    crate::files::embedded_lens(raw).or_else(|| {
        let db = database()?;
        let (w, h) = raw.developed_size();
        lookup(&db, &meta(), w as f64, h as f64, raw.orientation)
    })
}

/// Read-only local diagnosis, deliberately independent of any library or session.
pub fn inspect_file(path: &std::path::Path) -> Result<serde_json::Value, String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path).map_err(|e| e.to_string())?.take(256 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > 256 * 1024 * 1024 {
        return Err("diagnostic input exceeds 256 MiB".into());
    }
    let raw = lightcraft_raw::probe_info(&bytes).map_err(|e| e.to_string())?;
    let meta = crate::optics_metadata::extract(&bytes);
    let (w, h) = raw.developed_size();
    let embedded = crate::files::embedded_lens(&raw);
    let external = database().map(|db| inspect(&db, &meta, w as f64, h as f64, raw.orientation));
    let effective = embedded.as_ref().or_else(|| external.as_ref().and_then(|r| r.correction.as_ref()));
    Ok(serde_json::json!({
        "camera_make": meta.make, "camera_model": meta.model,
        "lens_make": meta.lens_make, "lens_model": meta.lens_model,
        "focal_length": meta.focal_length, "aperture": meta.f_number,
        "source": if embedded.is_some() { "embedded" } else if effective.is_some() { "external_profile" } else { "unavailable" },
        "available": {
            "geometric_warp": effective.is_some_and(|l| l.warp.is_some() || l.has_stages()),
            "vignetting": effective.is_some_and(|l| l.vignette.is_some()),
        },
        "external_profile": external,
        "database": database().map(|db| serde_json::json!({"cameras": db.cameras.len(), "profiles": db.lenses.len()})),
        "correction": effective,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_geom::Orientation;

    const DB: &str = r#"<lensdatabase>
      <camera><maker>Testcam</maker><model>TC-1</model><mount>T</mount><cropfactor>1.5</cropfactor></camera>
      <lens><maker>Testcam</maker><model>Testcam 18-55mm f/3.5-5.6</model><mount>T</mount><cropfactor>1.5</cropfactor>
        <calibration>
          <distortion model="poly3" focal="18" k1="-0.03" />
          <vignetting model="pa" focal="18" aperture="3.5" distance="1000" k1="-0.4" k2="0" k3="0" />
        </calibration>
      </lens></lensdatabase>"#;

    fn meta() -> lightcraft_meta::Metadata {
        lightcraft_meta::Metadata {
            make: Some("Testcam".into()),
            model: Some("TC-1".into()),
            lens_model: Some("18-55mm F3.5-5.6".into()),
            focal_length: Some(18.0),
            f_number: Some(3.5),
            ..Default::default()
        }
    }

    /// The log says where the database was read from and what it holds, and warns when it holds no lenses (#2511).
    #[test]
    fn the_log_names_the_folder_and_what_it_holds() {
        let dir = Path::new("/lensfun/db");
        let (level, line) = summary(dir, ENV, &LensDb::parse(DB).unwrap());
        assert_eq!(level, log::Level::Info);
        assert!(line.contains("lenses 1, cameras 1") && line.contains("/lensfun/db") && line.contains(ENV), "{line}");
        let (level, line) = summary(dir, "downloaded", &LensDb::parse("<lensdatabase></lensdatabase>").unwrap());
        assert_eq!(level, log::Level::Warn);
        assert!(line.contains("no lenses") && line.contains("downloaded") && line.contains("lenses 0, cameras"), "{line}");
    }

    #[test]
    fn exif_lens_is_found_and_converted() {
        let db = LensDb::parse(DB).unwrap();
        let lens = lookup(&db, &meta(), 6000.0, 4000.0, Orientation::Normal).unwrap();
        assert!(lens.warp.is_some() && lens.vignette.is_some());
        assert_eq!(lens.source, LensSource::Database, "the Optics panel says where it came from");
        assert_eq!(profile_name(&db, "Testcam TC-1", "18-55mm F3.5-5.6").as_deref(), Some("Testcam 18-55mm f/3.5-5.6"));
        assert_eq!(profile_name(&db, "Testcam TC-1", "70-200mm F4"), None);
        let rotated = lookup(&db, &meta(), 6000.0, 4000.0, Orientation::Rotate90).unwrap();
        assert_eq!(rotated.warp.unwrap().center, lightcraft_geom::Point::new(0.5, 0.5));
        // no lens name, no focal length, an unknown lens: nothing
        assert!(lookup(&db, &lightcraft_meta::Metadata { lens_model: None, ..meta() }, 6000.0, 4000.0, Orientation::Normal).is_none());
        assert!(lookup(&db, &lightcraft_meta::Metadata { focal_length: None, ..meta() }, 6000.0, 4000.0, Orientation::Normal).is_none());
        let other = lightcraft_meta::Metadata { lens_model: Some("70-200mm F4".into()), ..meta() };
        assert!(lookup(&db, &other, 6000.0, 4000.0, Orientation::Normal).is_none());
    }

    /// A Nikon DSLR raw names no lens, only its specification (maker note `0x0084`): it is looked up by the name
    /// built from it, and only a profile named exactly that is used, never another maker's lens that merely shares
    /// the focal range and aperture.
    #[test]
    fn lens_specification_needs_a_profile_named_by_it() {
        let nikon = lightcraft_meta::Metadata { lens_model: None, lens_spec: Some([18.0, 55.0, 3.5, 5.6]), ..meta() };
        let at = |db: &LensDb| lookup(db, &nikon, 6000.0, 4000.0, Orientation::Normal);
        // "Testcam 18-55mm f/3.5-5.6" fits the specification, but so would any maker's 18-55 f/3.5-5.6: refused
        assert!(at(&LensDb::parse(DB).unwrap()).is_none());
        let aliased = LensDb::parse(
            &DB.replace("<model>Testcam 18-55mm f/3.5-5.6</model>", "<model>Testcam 18-55mm f/3.5-5.6</model><model>18-55mm f/3.5-5.6</model>"),
        )
        .unwrap();
        let lens = at(&aliased).unwrap();
        assert!(lens.warp.is_some() && lens.vignette.is_some());
        assert_eq!(Some(lens), lookup(&aliased, &meta(), 6000.0, 4000.0, Orientation::Normal), "same profile as by its name");
        // a lens model, when the file has one, still decides
        let named_other = lightcraft_meta::Metadata { lens_model: Some("70-200mm F4".into()), ..nikon.clone() };
        assert!(lookup(&aliased, &named_other, 6000.0, 4000.0, Orientation::Normal).is_none());
        // Exact aliases establish eligibility, never permission to pick the first duplicate.
        let mut ambiguous = aliased;
        ambiguous.lenses.push(ambiguous.lenses[0].clone());
        assert_eq!(inspect(&ambiguous, &nikon, 6000.0, 4000.0, Orientation::Normal).resolution.status, r3d_optics::Status::AmbiguousProfile);
        assert!(at(&ambiguous).is_none());
    }

    #[test]
    fn selection_and_conversion_share_the_effective_frame_crop() {
        let xml = DB.replace("<calibration>", "<calibration cropfactor=\"1.611\">");
        let m = lightcraft_meta::Metadata { focal_length: Some(20.0), focal_length_35mm: Some(32.0), ..meta() };
        let db = LensDb::parse(&xml).unwrap();
        let report = inspect(&db, &m, 6000.0, 4000.0, Orientation::Normal);
        assert_eq!(report.resolution.status, r3d_optics::Status::NoMatch);
        assert_eq!(report.resolution.frame_crop_factor, Some(1.5));
        assert!(report.correction.is_none());
        // With both calibrations present, select the wider 1.5 profile and convert at the same body crop.
        let both = DB.replace(
            "</calibration>",
            "</calibration><calibration cropfactor=\"1.611\"><distortion model=\"poly3\" focal=\"18\" k1=\"-0.03\" /></calibration>",
        );
        let both = LensDb::parse(&both).unwrap();
        let selected = inspect(&both, &m, 6000.0, 4000.0, Orientation::Normal);
        assert_eq!(selected.resolution.profile_index, Some(0));
        assert_eq!(selected.frame_crop_factor, Some(1.5));
        let shot = Shot { focal: 20.0, aperture: Some(3.5), distance: None, crop_factor: selected.frame_crop_factor, width: 6000.0, height: 4000.0 };
        assert!(selected.correction.is_some());
        assert_eq!(selected.correction, both.lenses[0].correction(&shot).map(|lens| EmbeddedLens { source: LensSource::Database, ..lens }));
        // A genuine crop mode can use the narrower calibration, and conversion uses precisely that crop.
        let m = lightcraft_meta::Metadata { focal_length_35mm: Some(36.0), ..m };
        let report = inspect(&db, &m, 6000.0, 4000.0, Orientation::Normal);
        assert_eq!(report.resolution.status, r3d_optics::Status::Selected);
        assert_eq!(report.frame_crop_factor, Some(1.8));
        assert_eq!(report.frame_crop_factor, report.resolution.frame_crop_factor);
        let shot = Shot { focal: 20.0, aperture: Some(3.5), distance: None, crop_factor: Some(1.8), width: 6000.0, height: 4000.0 };
        assert!(report.correction.is_some());
        assert_eq!(report.correction, db.lenses[0].correction(&shot).map(|lens| EmbeddedLens { source: LensSource::Database, ..lens }));
    }

    #[test]
    fn xml_crop_validity_survives_the_adapter() {
        let inspect_xml = |xml: &str| inspect(&LensDb::parse(xml).unwrap(), &meta(), 6000.0, 4000.0, Orientation::Normal);
        let valid = inspect_xml(DB);
        assert_eq!(valid.resolution.status, r3d_optics::Status::Selected);
        assert!(valid.correction.is_some());
        // Missing/invalid camera crops must not turn into a trusted full-frame body.
        for value in [None, Some(""), Some("bad"), Some("NaN"), Some("inf"), Some("0"), Some("-1"), Some("0.09"), Some("21")] {
            let replacement = value.map(|value| format!("<cropfactor>{value}</cropfactor>")).unwrap_or_default();
            let xml = DB.replacen("<cropfactor>1.5</cropfactor>", &replacement, 1);
            let report = inspect_xml(&xml);
            assert_eq!(report.resolution.status, r3d_optics::Status::NoMatch, "body {value:?}");
            assert!(report.correction.is_none());
            // Invalid/missing parent crop also cannot establish calibration coverage.
            let xml = DB.replacen(
                "<mount>T</mount><cropfactor>1.5</cropfactor>\n        <calibration>",
                &format!("<mount>T</mount>{replacement}\n        <calibration>"),
                1,
            );
            assert_eq!(inspect_xml(&xml).resolution.status, r3d_optics::Status::NoMatch, "profile {value:?}");
        }
        // Omitting the calibration override inherits the explicit parent, preserving the existing correction.
        let inherited = inspect_xml(&DB.replace("<calibration>", "<calibration cropfactor=\"1.5\">"));
        assert_eq!(inherited.correction, valid.correction);
        for value in ["", "bad", "NaN", "inf", "0", "-1", "0.09", "21"] {
            let xml = DB.replace("<calibration>", &format!("<calibration cropfactor=\"{value}\">"));
            let report = inspect_xml(&xml);
            assert_eq!(report.resolution.status, r3d_optics::Status::NoMatch, "override {value:?}");
            assert!(report.correction.is_none());
        }
        // A valid override needs no parent; explicit full-frame crop is valid, never confused with absence.
        let xml = DB.replace("<cropfactor>1.5</cropfactor>", "").replace("<calibration>", "<calibration cropfactor=\"1\">");
        let db = LensDb::parse(&xml).unwrap();
        let m = lightcraft_meta::Metadata { focal_length_35mm: Some(18.0), ..meta() };
        let report = inspect(&db, &m, 6000.0, 4000.0, Orientation::Normal);
        assert_eq!(report.resolution.status, r3d_optics::Status::Selected);
        assert_eq!(report.frame_crop_factor, Some(1.0));
        assert!(report.correction.is_some());
        let xml = DB.replace("<cropfactor>1.5</cropfactor>", "<cropfactor>1</cropfactor>");
        assert_eq!(inspect_xml(&xml).resolution.status, r3d_optics::Status::Selected);
    }

    #[test]
    fn declared_camera_mount_compatibility_survives_database_merge() {
        let xml = DB.replacen("<mount>T</mount>", "<mount>Canon EF-S</mount>", 1).replace("<mount>T</mount>", "<mount>Canon EF</mount>");
        let mut db = LensDb::parse(&xml).unwrap();
        let status = |db: &LensDb| inspect(db, &meta(), 6000.0, 4000.0, Orientation::Normal).resolution.status;
        assert_eq!(status(&db), r3d_optics::Status::NoMatch);
        db.extend(
            LensDb::parse(
                r#"<lensdatabase>
          <mount><name>Canon EF-S</name><compat>Canon EF</compat></mount>
          <mount><name>Canon EF</name><compat>Third mount</compat></mount>
        </lensdatabase>"#,
            )
            .unwrap(),
        );
        let report = inspect(&db, &meta(), 6000.0, 4000.0, Orientation::Normal);
        assert_eq!(report.resolution.status, r3d_optics::Status::Selected);
        assert_eq!(report.distortion, ComponentStatus::Applied);
        assert_eq!(report.vignetting, ComponentStatus::Applied);
        // Only the camera mount's own declaration is used; no transitive traversal.
        db.lenses[0].mounts = vec!["Third mount".into()];
        assert_eq!(status(&db), r3d_optics::Status::NoMatch);
        db.cameras[0].mount = "Canon EF".into();
        db.lenses[0].mounts = vec!["Canon EF-S".into()];
        assert_eq!(status(&db), r3d_optics::Status::NoMatch);
        db.cameras[0].mount = "Unrelated mount".into();
        db.lenses[0].mounts = vec!["Canon EF".into()];
        assert_eq!(status(&db), r3d_optics::Status::NoMatch);
    }

    #[test]
    fn fixed_lens_mount_resolves_without_a_lens_name_and_rejects_duplicates() {
        let fixed = DB.replace("<mount>T</mount>", "<mount>testFixed</mount>");
        let mut db = LensDb::parse(&fixed).unwrap();
        let metadata = lightcraft_meta::Metadata { lens_model: None, ..meta() };
        let report = inspect(&db, &metadata, 6000.0, 4000.0, Orientation::Normal);
        assert!(report.resolution.via_fixed_lens);
        assert_eq!(report.distortion, ComponentStatus::Applied);
        assert_eq!(report.vignetting, ComponentStatus::Applied);
        assert_eq!(report.chromatic_aberration, ComponentStatus::Missing);
        db.lenses.push(db.lenses[0].clone());
        let report = inspect(&db, &metadata, 6000.0, 4000.0, Orientation::Normal);
        assert_eq!(report.resolution.status, r3d_optics::Status::AmbiguousProfile);
        assert!(report.correction.is_none());
    }

    #[test]
    fn reports_missing_components_and_unsupported_projection_accurately() {
        let mut db = LensDb::parse(DB).unwrap();
        db.lenses[0].vignetting.clear();
        let m = lightcraft_meta::Metadata { focal_length: None, ..meta() };
        let report = inspect(&db, &m, 6000.0, 4000.0, Orientation::Normal);
        assert_eq!(report.distortion, ComponentStatus::MissingMetadata);
        assert_eq!(report.vignetting, ComponentStatus::Missing);
        db.lenses[0].rectilinear = false;
        let report = inspect(&db, &meta(), 6000.0, 4000.0, Orientation::Normal);
        assert_eq!(report.distortion, ComponentStatus::UnsupportedProjection);
        assert!(report.correction.is_none());
    }

    #[test]
    fn missing_aperture_does_not_invent_a_shading_correction() {
        let db = LensDb::parse(DB).unwrap();
        let m = lightcraft_meta::Metadata { f_number: None, ..meta() };
        let report = inspect(&db, &m, 6000.0, 4000.0, Orientation::Normal);
        assert_eq!(report.distortion, ComponentStatus::Applied);
        assert_eq!(report.vignetting, ComponentStatus::MissingMetadata);
        assert!(report.correction.unwrap().vignette.is_none());
    }

    /// A full-frame body in APS-C / Super 35 mode (an ILCE-1M2 at 70 mm records 105 mm equivalent) frames only the
    /// middle of the image circle: the profile is applied at the frame's crop factor, not the body's, so the crop's
    /// corners don't get the full frame's corner correction.
    #[test]
    fn in_camera_crop_mode_uses_the_frames_crop_factor() {
        let db = LensDb::parse(
            r#"<lensdatabase>
              <camera><maker>Sony</maker><model>ILCE-1M2</model><mount>Sony E</mount><cropfactor>1</cropfactor></camera>
              <lens><maker>Sigma</maker><model>24-70mm F2.8 DG DN | Art 019</model><mount>Sony E</mount><cropfactor>1.0</cropfactor>
                <calibration>
                  <distortion model="ptlens" focal="70" a="0.0019947" b="0.0035263" c="0.0005016" />
                  <vignetting model="pa" focal="70" aperture="2.8" distance="1000" k1="-0.4" k2="0" k3="0" />
                </calibration>
              </lens></lensdatabase>"#,
        )
        .unwrap();
        let m = |f35: Option<f64>| lightcraft_meta::Metadata {
            make: Some("SONY".into()),
            model: Some("ILCE-1M2".into()),
            lens_model: Some("24-70mm F2.8 DG DN | Art 019".into()),
            focal_length: Some(70.0),
            focal_length_35mm: f35,
            f_number: Some(2.8),
            ..Default::default()
        };
        let at = |crop: f64, w: f64, h: f64| {
            let shot = Shot { focal: 70.0, aperture: Some(2.8), distance: None, crop_factor: Some(crop), width: w, height: h };
            EmbeddedLens { source: LensSource::Database, ..db.lenses[0].correction(&shot).unwrap() }
        };
        let aps_c = lookup(&db, &m(Some(105.0)), 5616.0, 3744.0, Orientation::Normal).unwrap();
        assert_eq!(aps_c, at(1.5, 5616.0, 3744.0));
        assert_ne!(aps_c, at(1.0, 5616.0, 3744.0), "the crop is not corrected as a full frame");
        // full frame, a 35 mm equivalent rounded off by a millimetre, or none at all: the body's crop factor
        for f35 in [Some(70.0), Some(71.0), None] {
            assert_eq!(lookup(&db, &m(f35), 8640.0, 5760.0, Orientation::Normal).unwrap(), at(1.0, 8640.0, 5760.0), "{f35:?}");
        }
        assert_eq!(recorded_crop_factor(70.0, Some(105.0)), Some(1.5), "no body in the database: the 35 mm equivalent's");
        assert_eq!(recorded_crop_factor(0.0, Some(105.0)), None, "a hostile focal length is ignored");
    }

    #[test]
    fn unknown_camera_crop_uses_exif_equivalent_without_full_frame_assumption() {
        for crop in [
            "",
            "<cropfactor/>",
            "<cropfactor>bad</cropfactor>",
            "<cropfactor>NaN</cropfactor>",
            "<cropfactor>inf</cropfactor>",
            "<cropfactor>0.049</cropfactor>",
            "<cropfactor>21</cropfactor>",
        ] {
            // A 0.8 calibration covers the 0.95 recorded frame. R3D refuses extrapolation
            // from the original 1.5 calibration to a wider frame.
            let xml = DB
                .replacen("<cropfactor>1.5</cropfactor>", crop, 1)
                .replace("<cropfactor>1.5</cropfactor>", "<cropfactor>0.8</cropfactor>")
                .replace("-0.03", "-0.003")
                .replace("-0.4", "-0.04");
            let db = LensDb::parse(&xml).unwrap();
            // A below-1 crop exposes the old synthetic 1.0 body: the 10% crop-mode policy would keep it.
            let m = lightcraft_meta::Metadata { focal_length_35mm: Some(17.1), ..meta() };
            let expected = db.lenses[0]
                .correction(&Shot { focal: 18.0, aperture: Some(3.5), distance: None, crop_factor: Some(17.1 / 18.0), width: 6000.0, height: 4000.0 })
                .map(|lens| EmbeddedLens { source: LensSource::Database, ..lens })
                .unwrap();
            assert_eq!(lookup(&db, &m, 6000.0, 4000.0, Orientation::Normal).unwrap(), expected, "{crop}");
            // With neither a trusted body nor recorded frame crop, R3D deliberately
            // refuses the profile instead of inventing a sensor size.
            assert!(lookup(&db, &meta(), 6000.0, 4000.0, Orientation::Normal).is_none(), "no equivalent: {crop}");
        }
    }

    /// A synthetic grid recorded through the profile's barrel distortion comes out of the pipeline's geometry
    /// stage with straight lines.
    #[test]
    fn pipeline_straightens_a_distorted_grid() {
        let db = LensDb::parse(DB).unwrap();
        let profile = &db.lenses[0];
        let (w, h) = (600usize, 400usize);
        let lens = lookup(&db, &meta(), w as f64, h as f64, Orientation::Normal).unwrap();
        // record the grid: each sensor pixel sees the scene point whose distorted position it is
        let n = h as f64 / 2.0;
        let undistort = |x: f64, y: f64| {
            let (dx, dy) = ((x - w as f64 / 2.0) / n, (y - h as f64 / 2.0) / n);
            let rd = dx.hypot(dy);
            let (mut lo, mut hi) = (0.0, 3.0);
            for _ in 0..60 {
                let mid = (lo + hi) / 2.0;
                if mid * profile.distortion_ratio(18.0, mid) < rd {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let s = if rd > 0.0 { lo / rd } else { 1.0 };
            (w as f64 / 2.0 + dx * s * n, h as f64 / 2.0 + dy * s * n)
        };
        // horizontal scene lines every 50 px (2 px wide, dark on light)
        let line = |v: f64| if v.rem_euclid(50.0) < 2.0 { 0.02f32 } else { 0.8 };
        let src = lightcraft_raster::Rgb32f::from_fn(w, h, |x, y| {
            let (_, sy) = undistort(x as f64 + 0.5, y as f64 + 0.5);
            [line(sy); 3]
        });
        // the dark row nearest y = 51 in each column, sub-pixel (centroid of darkness), as rendered
        let line_rows = |img: &lightcraft_raster::Rgba8| -> Vec<f64> {
            (20..w - 20)
                .step_by(10)
                .map(|x| {
                    let (mut sw, mut swy) = (0.0, 0.0);
                    for y in 35..70 {
                        let d = 255.0 - img.get(x, y)[1] as f64;
                        if d > 60.0 {
                            sw += d;
                            swy += d * (y as f64 + 0.5);
                        }
                    }
                    swy / sw.max(1e-9)
                })
                .collect()
        };
        let spread = |rows: &[f64]| rows.iter().cloned().fold(f64::MIN, f64::max) - rows.iter().cloned().fold(f64::MAX, f64::min);
        let req = lightcraft_pipeline::RenderRequest::fit(w, h);
        let mut s = lightcraft_develop::DevelopSettings::default();
        let info = lightcraft_pipeline::SourceInfo { lens: Some(lens), ..Default::default() };
        let before = spread(&line_rows(&lightcraft_pipeline::render(&src, &info, &s, &req).image));
        s.optics.lens_profile = true;
        let after = spread(&line_rows(&lightcraft_pipeline::render(&src, &info, &s, &req).image));
        assert!(before > 2.0, "the recorded line is curved: {before} px");
        // 8-bit output and a 2 px line: the centroid measurement itself is good to about a pixel
        assert!(after < 1.0 && after < before / 8.0, "corrected line bent by {after} px (uncorrected {before})");
    }
}
