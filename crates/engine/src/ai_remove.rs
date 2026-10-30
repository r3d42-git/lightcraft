//! Local AI Remove jobs. Results are drafts until an explicit accept command.
use crate::{EngineError, PhotoId, Result, Session};
use lightcraft_develop::{AiEdit, DevelopSettings, Spot};
use serde_json::{Value, json};

#[derive(Default)]
pub struct AiRemove {
    pub message: String,
    pub draft: Option<AiEdit>,
    preview_settings: Option<(std::sync::Arc<DevelopSettings>, u64)>,
    pub preview_original: bool,
    pub selection: Option<Spot>,
    selection_tag: Option<(PhotoId, std::sync::Arc<DevelopSettings>, u64)>,
    tag: Option<(PhotoId, std::sync::Arc<DevelopSettings>, u64)>,
    spot: Option<usize>,
    #[cfg(not(target_arch = "wasm32"))]
    job: Option<Job>,
}
#[cfg(not(target_arch = "wasm32"))]
struct Job {
    reply: std::sync::mpsc::Receiver<std::result::Result<AiEdit, String>>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
#[cfg(not(target_arch = "wasm32"))]
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}
impl AiRemove {
    pub fn busy(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.job.is_some()
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    }
}
fn error(s: impl ToString) -> EngineError {
    EngineError::Other(s.to_string())
}
impl Session {
    fn ai_remove_tag(&self) -> Option<(PhotoId, std::sync::Arc<DevelopSettings>, u64)> {
        let id = self.active()?;
        let d = self.develop_of(id)?;
        Some((id, d, self.library_generation()))
    }
    pub fn ai_remove_poll(&mut self) {
        if self.ai_remove.selection_tag.is_some()
            && !self.ai_remove.selection_tag.as_ref().zip(self.ai_remove_tag()).is_some_and(|((id, settings, generation), (now, current, g))| {
                *id == now && *generation == g && std::sync::Arc::ptr_eq(settings, &current)
            })
        {
            self.ai_remove.selection = None;
            self.ai_remove.selection_tag = None;
        }
        if self.ai_remove.tag.is_some()
            && !self
                .ai_remove
                .tag
                .as_ref()
                .zip(self.ai_remove_tag())
                .is_some_and(|((id, d, g), (now, settings, generation))| *id == now && *g == generation && std::sync::Arc::ptr_eq(d, &settings))
        {
            self.ai_remove_cancel();
            self.ai_remove.message = "AI result discarded because the photo or edits changed.".into();
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(result) = self.ai_remove.job.as_ref().and_then(|j| match j.reply.try_recv() {
            Ok(r) => Some(r),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err("AI worker stopped unexpectedly.".into())),
            Err(_) => None,
        }) {
            self.ai_remove.job = None;
            match result {
                Ok(edit) => {
                    self.ai_remove.draft = Some(edit);
                    self.ai_remove.message = "Result ready. Review the patch before accepting.".into();
                }
                Err(e) => {
                    self.ai_remove.message = e;
                    self.ai_remove.tag = None;
                }
            }
        }
    }
    /// The pending patch developed with the current photo's full look, never committed.
    /// The settings and their potentially large pixel hash are built only once per draft.
    pub fn ai_remove_preview_settings(&mut self) -> Option<std::sync::Arc<DevelopSettings>> {
        self.ai_remove_poll();
        let draft = self.ai_remove.draft.as_ref()?;
        if self.ai_remove.preview_settings.is_none() {
            let id = self.active()?;
            let mut settings = (*self.develop_of(id)?).clone();
            let mut edit = draft.clone();
            edit.id = settings.ai_edits.iter().map(|e| e.id).max().unwrap_or(0).checked_add(1)?;
            settings.ai_edits.push(edit);
            lightcraft_develop::ai_edit::validate_stack(&settings.ai_edits).ok()?;
            if let Some(i) = self.ai_remove.spot.filter(|i| *i < settings.spots.len()) {
                settings.spots.remove(i);
            }
            let hash = settings.hash64();
            self.ai_remove.preview_settings = Some((std::sync::Arc::new(settings), hash));
        }
        self.ai_remove.preview_settings.as_ref().map(|(settings, _)| settings.clone())
    }

    /// A transient loupe job: no view/thumbnail cache and no effect on export settings.
    pub fn ai_remove_preview_job(&mut self, id: PhotoId, width: usize, height: usize, apply_crop: bool) -> Option<crate::media::RenderJob> {
        if self.active()? != id || self.ai_remove.preview_original {
            return None;
        }
        let settings = self.ai_remove_preview_settings()?;
        let hash = self.ai_remove.preview_settings.as_ref()?.1;
        self.preview_job_shared(id, width, height, apply_crop, settings, hash)
    }

    fn ai_remove_cancel(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.ai_remove.job = None;
        }
        self.ai_remove.draft = None;
        self.ai_remove.preview_settings = None;
        self.ai_remove.preview_original = false;
        self.ai_remove.tag = None;
        self.ai_remove.message = "Cancelled.".into();
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn ai_remove_start(&mut self, backend: Option<&'static str>, model_id: Option<String>) -> Result<Value> {
        self.ai_remove_poll();
        if self.ai_remove.busy() {
            return Err(error("AI Remove is already running."));
        }
        if self.ai_models.busy() {
            return Err(error("Wait for model management to finish."));
        }
        let ai_root = crate::ai_models::root()?;
        let selected = crate::ai_models::selected(&ai_root, model_id.as_deref(), backend)?;
        let backend = selected.backend.clone();
        let selected_id = selected.id.clone();
        let id = self.active().ok_or_else(|| error("Select a photo."))?;
        let d = self.develop_of(id).unwrap_or_default();
        let (spot, index) = if let Some(selection) = self.ai_remove.selection.clone() {
            (selection, None)
        } else {
            let index = self.active_spot.ok_or_else(|| error("Paint an AI selection first."))?;
            (d.spots.get(index).cloned().ok_or_else(|| error("Select a spot first."))?, Some(index))
        };
        if d.ai_edits.len() >= 8 {
            return Err(error("This photo already has eight AI patches."));
        }
        let photo = self.catalog.photo(id).cloned().ok_or_else(|| error("Photo missing."))?;
        let source = self.media.source_ref(&photo, crate::media::SourceLevel::Full);
        let info = self.source_info(id);
        let settings = (*d).clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = cancel.clone();
        static NEXT_SEED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(42);
        let seed = if backend == "lama" { 0 } else { next_diffusion_seed(&NEXT_SEED) };
        std::thread::Builder::new()
            .name("ai-remove".into())
            .spawn(move || {
                let result = crate::guard::catch("AI Remove", || {
                    let _store_lock = crate::ai_models::store_lock(&ai_root).map_err(|e| e.to_string())?;
                    let selected = crate::ai_models::resolve(&ai_root, Some(&selected_id), Some(&backend), false).map_err(|e| e.to_string())?;
                    run_worker(&selected.runtime, &backend, Some(&selected), source, info, settings, spot, index.is_none(), seed, &stopped)
                })
                .unwrap_or_else(Err);
                let _ = tx.send(result);
            })
            .map_err(error)?;
        self.ai_remove = AiRemove {
            message: "AI Remove is working locally…".into(),
            draft: None,
            preview_settings: None,
            preview_original: false,
            tag: self.ai_remove_tag(),
            spot: index,
            selection: self.ai_remove.selection.clone(),
            selection_tag: self.ai_remove.selection_tag.clone(),
            job: Some(Job { reply: rx, cancel }),
        };
        Ok(json!({"running":true}))
    }
}

pub(crate) fn specs() -> Vec<crate::cmd::CommandSpec> {
    use crate::cmd::{always, cmd, has_active};
    vec![
        cmd!(query "aiRemove.select", "Paint AI Selection", [], None, "{points:[[x,y],…],size?,feather?,opacity?} — temporary AI mask; no photo edit", has_active, |s, p| {
            let selection = selection_param(p)?;
            let tag = s.ai_remove_tag().ok_or_else(|| error("Select a photo."))?;
            s.ai_remove_cancel();
            s.ai_remove.selection = Some(selection);
            s.ai_remove.selection_tag = Some(tag);
            s.ai_remove.message = "AI selection ready. Run AI Remove to create a preview.".into();
            Ok(json!({"selected":true}))
        }),
        cmd!(query "aiRemove.clearSelection", "Clear AI Selection", [], None, "{} — discard the temporary AI mask and draft", always, |s, _| {
            s.ai_remove_cancel();
            s.ai_remove.selection = None;
            s.ai_remove.selection_tag = None;
            Ok(json!({}))
        }),
        cmd!(query "aiRemove.start", "AI Remove", [], None, "{modelId?:installed revision ID,backend?:objectclear|lama|rorem} — starts a local background draft from the painted selection", has_active, |s, p| {
            let backend = if p.get("backend").is_some() { Some(backend_param(p)?) } else { None };
            let model_id = p.get("modelId").and_then(Value::as_str).map(str::to_owned);
            #[cfg(not(target_arch = "wasm32"))] { s.ai_remove_start(backend, model_id) }
            #[cfg(target_arch = "wasm32")] { let _ = (s, backend, model_id); Err(error("Local AI Remove is unavailable in the browser.")) }
        }),
        cmd!(query "aiRemove.status", "AI Remove Status", [], None, "{}", always, |s, _| { s.ai_remove_poll(); Ok(json!({"running":s.ai_remove.busy(),"ready":s.ai_remove.draft.is_some(),"selected":s.ai_remove.selection.is_some(),"previewOriginal":s.ai_remove.preview_original,"message":s.ai_remove.message})) }),
        cmd!(query "aiRemove.previewOriginal", "Show Photo Without AI Draft", [], None, "{enabled:bool} — compare the pending result without accepting it", has_active, |s, p| {
            s.ai_remove_poll();
            if s.ai_remove.draft.is_none() { return Err(error("No current AI result to preview.")); }
            let enabled = p.get("enabled").and_then(Value::as_bool).ok_or_else(|| error("Give enabled as a boolean."))?;
            s.ai_remove.preview_original = enabled;
            Ok(json!({"previewOriginal":enabled}))
        }),
        cmd!(query "aiRemove.cancel", "Cancel AI Remove", [], None, "{}", always, |s, _| { s.ai_remove_cancel(); Ok(json!({})) }),
        cmd!("aiRemove.accept", "Accept AI Remove", [], None, "{}", has_active, |s, _| {
            s.ai_remove_poll();
            let mut edit = s.ai_remove.draft.clone().ok_or_else(|| error("No current AI result to accept."))?;
            let id = s.active().ok_or_else(|| error("No active photo."))?;
            let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
            edit.id = d.ai_edits.iter().map(|e| e.id).max().unwrap_or(0).checked_add(1).ok_or_else(|| error("Too many edit IDs."))?;
            d.ai_edits.push(edit);
            lightcraft_develop::ai_edit::validate_stack(&d.ai_edits).map_err(error)?;
            if let Some(i) = s.ai_remove.spot.filter(|i| *i < d.spots.len()) {
                d.spots.remove(i);
            }
            s.set_develop(id, d, "AI Remove")?;
            s.active_spot = None;
            s.ai_remove_cancel();
            s.ai_remove.selection = None;
            s.ai_remove.selection_tag = None;
            s.ai_remove.message = "AI Remove accepted. Undo is available.".into();
            Ok(json!({}))
        }),
        cmd!("aiRemove.update", "Update AI Remove", [], None, "{id:u32,enabled?:bool,delete?:bool}", has_active, |s, p| {
            let id = s.active().ok_or_else(|| error("No photo."))?;
            let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
            let edit_id = p.get("id").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).ok_or_else(|| error("Missing patch ID."))?;
            let i = d.ai_edits.iter().position(|e| e.id == edit_id).ok_or_else(|| error("Patch missing."))?;
            if p.get("delete").and_then(Value::as_bool) == Some(true) {
                d.ai_edits.remove(i);
            } else if let Some(enabled) = p.get("enabled").and_then(Value::as_bool) {
                if let Some(e) = d.ai_edits.get_mut(i) {
                    e.enabled = enabled;
                }
            } else {
                return Err(error("Give enabled or delete."));
            }
            s.set_develop(id, d, "Edit AI Remove")?;
            Ok(json!({}))
        }),
    ]
}

fn selection_param(params: &Value) -> Result<Spot> {
    let points = params
        .get("points")
        .and_then(Value::as_array)
        .filter(|p| !p.is_empty() && p.len() <= 4096)
        .ok_or_else(|| error("Paint between 1 and 4096 AI selection points."))?;
    let mut selected = Vec::with_capacity(points.len());
    for point in points {
        let pair = point.as_array().filter(|p| p.len() == 2).ok_or_else(|| error("Invalid AI selection point."))?;
        let x = pair
            .first()
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
            .ok_or_else(|| error("Invalid AI selection point."))?;
        let y = pair
            .get(1)
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
            .ok_or_else(|| error("Invalid AI selection point."))?;
        selected.push(lightcraft_geom::Point::new(x, y));
    }
    let bounded = |name: &str, default: f64, min: f64, max: f64| -> Result<f64> {
        match params.get(name) {
            None => Ok(default),
            Some(v) => v.as_f64().filter(|v| v.is_finite() && (min..=max).contains(v)).ok_or_else(|| error(format!("Invalid AI selection {name}."))),
        }
    };
    Ok(Spot {
        points: selected,
        size: bounded("size", 0.02, 0.001, 0.25)?,
        feather: bounded("feather", 50.0, 0.0, 100.0)?,
        opacity: bounded("opacity", 100.0, 0.0, 100.0)?,
        source_offset: None,
        ..Spot::default()
    })
}

fn backend_param(params: &Value) -> Result<&'static str> {
    match params.get("backend") {
        None => Ok("objectclear"),
        Some(Value::String(name)) if name == "objectclear" => Ok("objectclear"),
        Some(Value::String(name)) if name == "lama" => Ok("lama"),
        Some(Value::String(name)) if name == "rorem" => Ok("rorem"),
        _ => Err(error("Invalid AI Remove backend. Choose objectclear, lama or rorem.")),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn backend_directory(backend: &str) -> Option<&'static str> {
    match backend {
        "objectclear" => Some("objectclear"),
        "lama" => Some("lama"),
        "rorem" => Some("model"),
        _ => None,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn next_diffusion_seed(counter: &std::sync::atomic::AtomicU32) -> u32 {
    // Skip zero even when the counter wraps after u32::MAX.
    counter
        .fetch_update(std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed, |seed| Some(seed.wrapping_add(1).max(1)))
        .unwrap_or(42)
        .max(1)
}

/// Use equal context on both axes, shifting at edges instead of clipping it.
/// Very narrow originals retain all available context on the shorter axis.
#[cfg(not(target_arch = "wasm32"))]
fn context_rect(bounds: [f64; 4], width: usize, height: usize) -> Option<[f64; 4]> {
    let [left, top, right, bottom] = bounds;
    let (width, height) = (width as f64, height as f64);
    if !bounds.iter().all(|v| v.is_finite())
        || right <= left
        || bottom <= top
        || width == 0.0
        || height == 0.0
        || right <= 0.0
        || bottom <= 0.0
        || left >= width
        || top >= height
    {
        return None;
    }
    let half = (right - left).max(bottom - top).max(256.0);
    let side = (half * 2.0).ceil();
    let (w, h) = (side.min(width), side.min(height));
    let x = (((left + right) * 0.5 - w * 0.5).floor()).clamp(0.0, width - w);
    let y = (((top + bottom) * 0.5 - h * 0.5).floor()).clamp(0.0, height - h);
    Some([x, y, x + w, y + h])
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn run_worker(
    runtime: &std::path::Path,
    backend: &str,
    package: Option<&crate::ai_models::Resolved>,
    source: crate::media::SourceRef,
    info: lightcraft_pipeline::SourceInfo,
    d: DevelopSettings,
    spot: Spot,
    continuous: bool,
    seed: u32,
    cancel: &std::sync::atomic::AtomicBool,
) -> std::result::Result<AiEdit, String> {
    use std::{fs, sync::atomic::Ordering, time::Duration};
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("lightcraft-ai-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&dir).map_err(|e| e.to_string())?;
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _scratch = Scratch(dir.clone());
    let work = || -> std::result::Result<AiEdit, String> {
        let decoded = source.load_source()?;
        if decoded.from_proxy {
            return Err("AI Remove needs the original photo online.".into());
        }
        let edited = lightcraft_pipeline::ai_edits::source_with_edits(&decoded.image, &d);
        let full = edited.as_ref();
        let frame = lightcraft_pipeline::frame_for(full, decoded.info.as_ref().unwrap_or(&info), &d, false);
        let radius = spot.size * full.width.max(full.height) as f64;
        if !radius.is_finite() || radius <= 0.0 || spot.points.is_empty() || spot.points.len() > 4096 {
            return Err("Invalid brush mask.".into());
        }
        let mut bounds = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for p in &spot.points {
            if !p.x.is_finite() || !p.y.is_finite() {
                return Err("Invalid brush point.".into());
            }
            let (x, y) = (p.x * frame.ow, p.y * frame.oh);
            bounds[0] = bounds[0].min(x - radius);
            bounds[1] = bounds[1].min(y - radius);
            bounds[2] = bounds[2].max(x + radius);
            bounds[3] = bounds[3].max(y + radius);
        }
        // Bound the entire transformed brush rectangle through the nonlinear warp. Mapping
        // only centres (or a few boundary points) could clip curved capsule support.
        let oriented = frame.warp.as_ref().map_or(Some(bounds), |warp| warp.source_bounds(bounds)).unwrap_or([0.0, 0.0, frame.ow, frame.oh]);
        bounds = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for (x, y) in [(oriented[0], oriented[1]), (oriented[0], oriented[3]), (oriented[2], oriented[1]), (oriented[2], oriented[3])] {
            let (x, y) = d.orientation.inverse().map(x, y, frame.ow, frame.oh);
            bounds[0] = bounds[0].min(x);
            bounds[1] = bounds[1].min(y);
            bounds[2] = bounds[2].max(x);
            bounds[3] = bounds[3].max(y);
        }
        let support = lightcraft_pipeline::ai_edits::brush_support(&frame, &spot, continuous);
        let brush = lightcraft_pipeline::ai_edits::BrushSupport::new(&support, full.width, full.height);
        let [x0, y0, x1, y1] = context_rect(bounds, full.width, full.height).ok_or_else(|| "The mask is outside the photo.".to_string())?;
        let scale = (1024.0 / (x1 - x0).max(y1 - y0)).min(1.0);
        let (w, h) = (((x1 - x0) * scale).ceil() as usize, ((y1 - y0) * scale).ceil() as usize);
        let rect = [x0 / full.width as f64, y0 / full.height as f64, x1 / full.width as f64, y1 / full.height as f64];
        let mut src = lightcraft_raster::Rgb32f::new(w, h);
        for (i, p) in src.data.iter_mut().enumerate() {
            *p = full.sample_bilinear(
                (x0 + ((i % w) as f64 + 0.5) * (x1 - x0) / w as f64 - 0.5) as f32,
                (y0 + ((i / w) as f64 + 0.5) * (y1 - y0) / h as f64 - 0.5) as f32,
            );
        }
        let mut input = Vec::with_capacity(w.saturating_mul(h).saturating_mul(12));
        for p in &src.data {
            for v in p {
                input.extend_from_slice(&v.to_le_bytes());
            }
        }
        let mut mask = vec![0u8; w.saturating_mul(h)];
        // Evaluate exact frozen transformed support at each source-space inference sample.
        for (i, m) in mask.iter_mut().enumerate() {
            if i % 1024 == 0 && cancel.load(Ordering::Relaxed) {
                return Err("Cancelled.".into());
            }
            let point = [
                (x0 + ((i % w) as f64 + 0.5) * (x1 - x0) / w as f64) / full.width as f64,
                (y0 + ((i / w) as f64 + 0.5) * (y1 - y0) / h as f64) / full.height as f64,
            ];
            if brush.alpha(point, full.width, full.height) > 0.0 {
                *m = 255;
            }
        }
        if !mask.contains(&255) {
            return Err("The selected mask is empty.".into());
        }
        fs::write(dir.join("input.rgb"), input).map_err(|e| e.to_string())?;
        fs::write(dir.join("mask.u8"), &mask).map_err(|e| e.to_string())?;
        let request = json!({"version":1,"input":dir.join("input.rgb"),"mask":dir.join("mask.u8"),"output":dir.join("output.rgba"),"width":w,"height":h,"rect":rect,"backend":backend,"model_dir":package.map(|p| p.model.clone()).unwrap_or_else(|| runtime.join(backend_directory(backend).unwrap_or(""))),"model_package":package.map(|p|json!({"path":p.model,"manifest":p.manifest})),"seed":seed,"steps":20});
        fs::write(dir.join("request.json"), serde_json::to_vec(&request).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled.".into());
        }
        let mut child = std::process::Command::new(runtime.join("bin/python3"))
            .arg("-I")
            .arg("-B")
            .arg(package.map(|p| p.worker.clone()).unwrap_or_else(|| runtime.join("worker.py")))
            .arg(dir.join("request.json"))
            .arg(dir.join("response.json"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Cannot start AI Remove: {e}"))?;
        if let Err(failure) = wait_worker(&mut child, cancel, Duration::from_secs(600)) {
            if !cancel.load(Ordering::Relaxed) {
                let response = dir.join("response.json");
                if fs::metadata(&response).is_ok_and(|m| m.len() <= 65536)
                    && let Ok(bytes) = fs::read(response)
                    && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                    && let Some(detail) = value.get("error").and_then(Value::as_str)
                {
                    return Err(format!("AI Remove failed: {}", detail.chars().take(1000).collect::<String>()));
                }
            }
            return Err(failure);
        }
        let response_path = dir.join("response.json");
        if fs::metadata(&response_path).map_err(|e| e.to_string())?.len() > 65536 {
            return Err("Worker response is too large.".into());
        }
        let response: Value = serde_json::from_slice(&fs::read(response_path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        if response.get("version").and_then(Value::as_u64) != Some(1)
            || response.get("width").and_then(Value::as_u64) != Some(w as u64)
            || response.get("height").and_then(Value::as_u64) != Some(h as u64)
        {
            return Err("AI worker returned mismatched dimensions.".into());
        }
        let output = dir.join("output.rgba");
        let expected = w.saturating_mul(h).saturating_mul(16);
        if fs::metadata(&output).map_err(|e| e.to_string())?.len() != expected as u64 {
            return Err("AI worker returned invalid pixel data.".into());
        }
        let raw = fs::read(output).map_err(|e| e.to_string())?;
        let mut pixels = Vec::with_capacity(mask.len());
        for (i, chunk) in raw.chunks_exact(16).enumerate() {
            let mut p = [0.0f32; 4];
            for (v, bytes) in p.iter_mut().zip(chunk.chunks_exact(4)) {
                let a = <[u8; 4]>::try_from(bytes).map_err(|e| e.to_string())?;
                *v = f32::from_le_bytes(a);
            }
            // Never trust worker alpha: only the exact user-authorized source mask can change pixels.
            p[3] = f32::from(mask.get(i).copied().unwrap_or(0)) / 255.0;
            if p[3] == 0.0
                && let Some(original) = src.data.get(i)
            {
                p[0] = original[0];
                p[1] = original[1];
                p[2] = original[2];
            }
            pixels.push(p);
        }
        AiEdit::from_pixels(
            AiEdit {
                id: 0,
                enabled: true,
                rect,
                support: Some(support),
                width: w as u32,
                height: h as u32,
                pixels: String::new(),
                model: response.get("model").and_then(Value::as_str).ok_or("Worker omitted model provenance.")?.into(),
                model_version: response
                    .get("model_version")
                    .or_else(|| response.get("revision"))
                    .and_then(Value::as_str)
                    .ok_or("Worker omitted model revision.")?
                    .into(),
                seed: u64::from(seed),
            },
            &pixels,
        )
        .map_err(|e| e.to_string())
    };
    work()
}

#[cfg(not(target_arch = "wasm32"))]
fn wait_worker(
    child: &mut std::process::Child,
    cancel: &std::sync::atomic::AtomicBool,
    timeout: std::time::Duration,
) -> std::result::Result<(), String> {
    use std::{sync::atomic::Ordering, time::Duration};
    let started = std::time::Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) || started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(if cancel.load(Ordering::Relaxed) { "Cancelled." } else { "AI Remove timed out after ten minutes." }.into());
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => return Err("The local AI worker failed. Check the model/runtime setup.".into()),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn draft(s: &mut Session) {
        let edit = AiEdit::from_pixels(
            AiEdit {
                id: 0,
                enabled: true,
                rect: [0.0, 0.0, 1.0, 1.0],
                support: None,
                width: 1,
                height: 1,
                pixels: String::new(),
                model: "test".into(),
                model_version: "1".into(),
                seed: 0,
            },
            &[[0.2, 0.3, 0.4, 1.0]],
        )
        .unwrap();
        s.ai_remove.tag = s.ai_remove_tag();
        s.ai_remove.draft = Some(edit);
        s.ai_remove.spot = Some(0);
    }
    #[test]
    #[cfg(unix)]
    fn active_lens_corrections_reach_worker_without_disabling_settings() {
        use std::os::unix::fs::PermissionsExt;
        struct Runtime(std::path::PathBuf);
        impl Runtime {
            fn path(&self) -> &std::path::Path {
                &self.0
            }
        }
        impl Drop for Runtime {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let runtime = Runtime(std::env::temp_dir().join(format!("lc-ai-geometry-test-{}", std::process::id())));
        std::fs::create_dir(runtime.path()).unwrap();
        std::fs::create_dir(runtime.path().join("bin")).unwrap();
        let launcher = runtime.path().join("bin/python3");
        // A deterministic protocol worker proves optics reaches inference without a model.
        // Deliberately overbroad worker alpha must be replaced by the authorized mask.
        let pixels: Vec<u8> = (0..128 * 96).flat_map(|_| [0.8f32, 0.7, 0.6, 1.0].into_iter().flat_map(f32::to_le_bytes)).collect();
        std::fs::write(runtime.path().join("pixels.rgba"), pixels).unwrap();
        std::fs::write(
            &launcher,
            r#"#!/bin/sh
cp "$4" "$(dirname "$0")/../captured-request.json"
cp "$(dirname "$0")/../pixels.rgba" "$(dirname "$5")/output.rgba"
printf '%s' '{"version":1,"width":128,"height":96,"model":"test","model_version":"geometry-1"}' > "$5"
"#,
        )
        .unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut settings = DevelopSettings::default();
        settings.optics.lens_profile = true;
        settings.optics.remove_ca = true;
        settings.optics.ca_red = 30.0;
        settings.optics.vignetting = 40.0;
        let lens = lightcraft_develop::EmbeddedLens {
            warp: Some(lightcraft_develop::EmbeddedWarp {
                planes: [[1.0, 0.18, 0.0, 0.0, 0.0, 0.0]; 3],
                center: lightcraft_geom::Point::new(0.5, 0.5),
                radius: 0.5,
            }),
            ..Default::default()
        };
        let info = lightcraft_pipeline::SourceInfo { lens: Some(lens), ..Default::default() };
        let original = settings.clone();
        let source = crate::media::SourceRef::Loaded(Box::new(crate::media::DecodedSource::new(
            std::sync::Arc::new(lightcraft_raster::Rgb32f::from_fn(128, 96, |_, _| [0.1, 0.2, 0.3])),
            Some(info.clone()),
        )));
        let spot = Spot { points: vec![lightcraft_geom::Point::new(0.8, 0.25)], size: 0.04, ..Default::default() };
        let package = crate::ai_models::Resolved {
            id: "lama-test-revision".into(),
            runtime: runtime.path().into(),
            model: runtime.path().join("separate-model"),
            worker: runtime.path().join("trusted-worker.py"),
            backend: "lama".into(),
            manifest: json!({"id":"lama-test-revision"}),
        };
        let edit = run_worker(
            runtime.path(),
            "lama",
            Some(&package),
            source,
            info,
            settings.clone(),
            spot,
            true,
            0,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        let request: Value = serde_json::from_slice(&std::fs::read(runtime.path().join("captured-request.json")).unwrap()).unwrap();
        assert_eq!(request["model_dir"], json!(package.model));
        assert_eq!(request["model_package"]["manifest"]["id"], "lama-test-revision");
        assert!(edit.support.as_ref().unwrap().geometry.is_some());
        assert_eq!(edit.model, "test");
        let mut patched = settings.clone();
        patched.ai_edits.push(edit);
        let src = lightcraft_raster::Rgb32f::from_fn(128, 96, |_, _| [0.1, 0.2, 0.3]);
        let result = lightcraft_pipeline::ai_edits::source_with_edits(&src, &patched);
        assert!(result.data.iter().any(|p| p[0] > 0.5));
        assert_eq!(result.get(0, 0), src.get(0, 0));
        assert_eq!(settings, original);
    }
    #[test]
    fn invalid_backend_preserves_current_draft_and_edits() {
        let mut s = Session::with_demo();
        draft(&mut s);
        let id = s.active().unwrap();
        let before = s.develop_of(id).unwrap();
        for backend in [json!("other"), json!(null), json!(12), json!("../lama")] {
            let e = s.execute("aiRemove.start", &json!({"backend":backend})).unwrap_err();
            assert!(e.to_string().contains("Invalid AI Remove backend"));
            assert!(s.ai_remove.draft.is_some());
            assert!(std::sync::Arc::ptr_eq(&before, &s.develop_of(id).unwrap()));
        }
        assert_eq!(backend_param(&json!({})).unwrap(), "objectclear");
        assert_eq!(backend_param(&json!({"backend":"objectclear"})).unwrap(), "objectclear");
        assert_eq!(backend_param(&json!({"backend":"lama"})).unwrap(), "lama");
        assert_eq!(backend_param(&json!({"backend":"rorem"})).unwrap(), "rorem");
    }
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn model_directories_and_diffusion_seeds_are_bounded() {
        assert_eq!(backend_directory("objectclear"), Some("objectclear"));
        assert_eq!(backend_directory("lama"), Some("lama"));
        assert_eq!(backend_directory("rorem"), Some("model"));
        assert_eq!(backend_directory("../objectclear"), None);
        let counter = std::sync::atomic::AtomicU32::new(42);
        assert_eq!(next_diffusion_seed(&counter), 42);
        assert_eq!(next_diffusion_seed(&counter), 43);
        counter.store(u32::MAX, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(next_diffusion_seed(&counter), u32::MAX);
        assert_eq!(next_diffusion_seed(&counter), 1);
        assert_eq!(next_diffusion_seed(&counter), 2);
    }
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn context_is_square_and_shifts_at_edges_without_losing_selection() {
        for bounds in [[970.0, 770.0, 990.0, 790.0], [10.0, 10.0, 30.0, 30.0], [400.0, 300.0, 600.0, 350.0]] {
            let [x0, y0, x1, y1] = context_rect(bounds, 1000, 800).unwrap();
            assert_eq!(x1 - x0, y1 - y0);
            assert!(x0 <= bounds[0] && y0 <= bounds[1] && x1 >= bounds[2] && y1 >= bounds[3]);
            assert!(x0 >= 0.0 && y0 >= 0.0 && x1 <= 1000.0 && y1 <= 800.0);
        }
        // A larger selection receives twice its longest dimension in total context.
        assert_eq!(context_rect([500.0, 500.0, 600.0, 900.0], 2000, 2000).unwrap(), [150.0, 300.0, 950.0, 1100.0]);
        assert_eq!(context_rect([5.0, 400.0, 15.0, 420.0], 200, 1000).unwrap(), [0.0, 154.0, 200.0, 666.0]);
        assert_eq!(context_rect([400.0, 5.0, 420.0, 15.0], 1000, 200).unwrap(), [154.0, 0.0, 666.0, 200.0]);
        assert!(context_rect([1100.0, 1.0, 1200.0, 2.0], 1000, 800).is_none());
        assert!(context_rect([f64::NAN, 1.0, 2.0, 3.0], 1000, 800).is_none());
    }
    #[test]
    fn draft_accept_is_one_undoable_change_and_needs_explicit_accept() {
        let mut s = Session::with_demo();
        s.execute("spot.add", &json!({"points":[[0.5,0.5]],"size":0.01})).unwrap();
        draft(&mut s);
        let id = s.active().unwrap();
        assert!(s.develop_of(id).unwrap().ai_edits.is_empty());
        s.execute("aiRemove.accept", &json!({})).unwrap();
        let d = s.develop_of(id).unwrap();
        assert_eq!(d.ai_edits.len(), 1);
        assert!(d.spots.is_empty());
        s.execute("edit.undo", &json!({})).unwrap();
        let d = s.develop_of(id).unwrap();
        assert!(d.ai_edits.is_empty());
        assert_eq!(d.spots.len(), 1);
    }
    #[test]
    fn draft_preview_is_shared_developed_and_never_committed() {
        let mut s = Session::with_demo();
        s.execute("spot.add", &json!({"points":[[0.3,0.5]],"size":0.01})).unwrap();
        s.execute("spot.add", &json!({"points":[[0.7,0.5]],"size":0.02})).unwrap();
        s.execute("develop.set", &json!({"control":"light.exposure","value":0.7})).unwrap();
        let id = s.active().unwrap();
        let before = s.develop_of(id).unwrap();
        let history = s.catalog.photo(id).unwrap().history.len();
        let undo = s.undo.len();
        draft(&mut s);
        let first = s.ai_remove_preview_settings().unwrap();
        let second = s.ai_remove_preview_settings().unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &second), "one settings allocation per draft");
        assert_eq!(first.light.exposure, before.light.exposure);
        assert_eq!(first.wb, before.wb);
        assert_eq!(first.geometry, before.geometry);
        assert_eq!(first.spots, vec![before.spots[1].clone()], "only selected spot is replaced");
        assert_eq!(first.ai_edits.len(), before.ai_edits.len() + 1);
        let job = s.ai_remove_preview_job(id, 800, 600, true).unwrap();
        let repeated = s.ai_remove_preview_job(id, 800, 600, true).unwrap();
        assert!(std::sync::Arc::ptr_eq(&job.settings, &first));
        assert_eq!(job.key, repeated.key);
        assert!(job.cache.is_none() && job.view_cache.is_none());
        let normal = s.render_job(id, 800, 600, false, true).unwrap();
        assert!(std::sync::Arc::ptr_eq(&normal.settings, &before), "normal/export settings remain the catalogue's");
        assert_ne!(job.key, normal.key);
        assert!(std::sync::Arc::ptr_eq(&s.develop_of(id).unwrap(), &before));
        assert_eq!(s.catalog.photo(id).unwrap().history.len(), history);
        assert_eq!(s.undo.len(), undo);
        s.execute("aiRemove.previewOriginal", &json!({"enabled":true})).unwrap();
        assert!(s.ai_remove_preview_job(id, 800, 600, true).is_none());
        s.execute("aiRemove.previewOriginal", &json!({"enabled":false})).unwrap();
        assert!(std::sync::Arc::ptr_eq(&s.ai_remove_preview_settings().unwrap(), &first));
        s.execute("aiRemove.cancel", &json!({})).unwrap();
        assert!(s.ai_remove.preview_settings.is_none());
        assert!(s.ai_remove_preview_job(id, 800, 600, true).is_none());
        assert_eq!(s.undo.len(), undo);
    }
    #[test]
    fn draft_preview_cache_is_cleared_when_its_source_becomes_stale() {
        let mut s = Session::with_demo();
        for stale_kind in 0..3 {
            draft(&mut s);
            assert!(s.ai_remove_preview_settings().is_some());
            match stale_kind {
                0 => {
                    s.execute("develop.set", &json!({"control":"light.exposure","value":0.5})).unwrap();
                }
                1 => {
                    s.execute("library.down", &json!({})).unwrap();
                }
                _ => {
                    s.ai_remove.tag.as_mut().unwrap().2 = s.library_generation().wrapping_add(1);
                }
            }
            s.ai_remove_poll();
            assert!(s.ai_remove.draft.is_none());
            assert!(s.ai_remove.preview_settings.is_none());
            assert!(s.ai_remove_preview_settings().is_none());
        }
    }
    #[test]
    fn ai_selection_is_private_and_invalid_requests_preserve_it() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        let before = s.develop_of(id).unwrap();
        let undo = s.undo.len();
        s.execute("aiRemove.select", &json!({"points":[[0.4,0.3],[0.4,0.7]],"size":0.03})).unwrap();
        assert!(std::sync::Arc::ptr_eq(&before, &s.develop_of(id).unwrap()));
        assert_eq!(s.undo.len(), undo);
        let selected = s.ai_remove.selection.clone();
        draft(&mut s);
        for invalid in [
            json!({"points":[]}),
            json!({"points":[[2,0.5]]}),
            json!({"points":[[0.5,null]]}),
            json!({"points":[[0.5,0.5,0.5]]}),
            json!({"points":[[0.5,0.5]],"size":0}),
            json!({"points":[[0.5,0.5]],"feather":"bad"}),
            json!({"points":vec![[0.5,0.5];4097]}),
        ] {
            assert!(s.execute("aiRemove.select", &invalid).is_err());
            assert_eq!(s.ai_remove.selection, selected);
            assert!(s.ai_remove.draft.is_some());
        }
        s.execute("aiRemove.cancel", &json!({})).unwrap();
        assert_eq!(s.ai_remove.selection, selected, "discard keeps mask for another variant");
        s.execute("aiRemove.clearSelection", &json!({})).unwrap();
        assert!(s.ai_remove.selection.is_none());
        assert_eq!(s.undo.len(), undo);
    }
    #[test]
    fn accepting_ai_selection_keeps_classic_spots_and_clears_mask() {
        let mut s = Session::with_demo();
        s.execute("spot.add", &json!({"points":[[0.2,0.5]],"size":0.01})).unwrap();
        let id = s.active().unwrap();
        let original = s.develop_of(id).unwrap();
        s.execute("aiRemove.select", &json!({"points":[[0.7,0.5]],"size":0.02})).unwrap();
        draft(&mut s);
        s.ai_remove.spot = None; // AI selection jobs never capture a classic spot index.
        let preview = s.ai_remove_preview_settings().unwrap();
        assert_eq!(preview.spots, original.spots);
        s.execute("aiRemove.accept", &json!({})).unwrap();
        let accepted = s.develop_of(id).unwrap();
        assert_eq!(accepted.spots, original.spots);
        assert_eq!(accepted.ai_edits, preview.ai_edits);
        assert!(s.ai_remove.selection.is_none());
        assert!(s.ai_remove.selection_tag.is_none());
    }
    #[test]
    fn ai_selection_is_invalidated_by_photo_edits_or_library_changes() {
        for kind in 0..3 {
            let mut s = Session::with_demo();
            s.execute("aiRemove.select", &json!({"points":[[0.5,0.5]]})).unwrap();
            match kind {
                0 => {
                    s.execute("develop.set", &json!({"control":"light.exposure","value":0.7})).unwrap();
                }
                1 => {
                    s.execute("library.down", &json!({})).unwrap();
                }
                _ => {
                    s.ai_remove.selection_tag.as_mut().unwrap().2 = s.library_generation().wrapping_add(1);
                }
            }
            s.ai_remove_poll();
            assert!(s.ai_remove.selection.is_none());
            assert!(s.ai_remove.selection_tag.is_none());
        }
    }
    #[test]
    fn ai_patch_never_auto_syncs_to_other_photos() {
        let mut s = Session::with_demo();
        let photos = s.visible_cloned();
        let first = photos[0];
        let other = photos[1];
        s.selection.active = Some(first);
        s.selection.ids = vec![first, other];
        s.auto_sync = true;
        draft(&mut s);
        s.execute("aiRemove.accept", &json!({})).unwrap();
        assert_eq!(s.develop_of(first).unwrap().ai_edits.len(), 1);
        assert!(s.develop_of(other).unwrap().ai_edits.is_empty());
    }
    #[test]
    fn changes_and_photo_switches_discard_pending_drafts() {
        let mut s = Session::with_demo();
        draft(&mut s);
        s.execute("develop.set", &json!({"control":"light.exposure","value":0.5})).unwrap();
        assert!(s.ai_remove.draft.is_none());
        assert!(s.execute("aiRemove.accept", &json!({})).is_err());
        draft(&mut s);
        s.execute("library.down", &json!({})).unwrap();
        assert!(s.ai_remove.draft.is_none());
    }
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn worker_cancel_and_timeout_reap_the_process() {
        use std::{process::Command, sync::atomic::AtomicBool, time::Duration};
        for cancelled in [true, false] {
            let mut child = Command::new("/usr/bin/python3").args(["-c", "import time; time.sleep(30)"]).spawn().unwrap();
            let result = wait_worker(&mut child, &AtomicBool::new(cancelled), Duration::from_millis(10));
            assert!(result.is_err());
            assert!(child.try_wait().unwrap().is_some());
        }
    }
}
