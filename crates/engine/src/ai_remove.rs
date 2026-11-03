//! Local AI Remove jobs. Results are drafts until an explicit accept command.
use crate::{EngineError, PhotoId, Result, Session};
use lightcraft_develop::{AiEdit, DevelopSettings, Spot};
pub use object::{ObjectSelection, SelectionMask};
use serde_json::{Value, json};

#[cfg(not(target_arch = "wasm32"))]
mod input;
mod object;
#[cfg(not(target_arch = "wasm32"))]
mod patch;
#[cfg(not(target_arch = "wasm32"))]
mod protocol;
#[cfg(not(target_arch = "wasm32"))]
mod runner;
#[cfg(all(test, not(target_arch = "wasm32")))]
use input::context_rect;
#[cfg(all(test, not(target_arch = "wasm32")))]
use protocol::{backend_directory, wait_worker};

/// Observable job lifecycle. Cancellation remains pending until the worker acknowledges completion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    #[default]
    Idle,
    Preparing,
    Loading,
    Running,
    Effects,
    Reconstruction,
    Finalizing,
    Ready,
    Failed,
    Cancelling,
    Cancelled,
}

#[derive(Default)]
pub struct AiRemove {
    pub include_effects: Option<bool>,
    pub reconstruct: Option<bool>,
    reconstruction_active: bool,
    effects_active: bool,
    pub object_selection: Option<ObjectSelection>,
    pub selecting: bool,
    selection_revision: u64,
    pub phase: Phase,
    cancel_reason: Option<&'static str>,
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
    phases: std::sync::mpsc::Receiver<Phase>,
    reply: std::sync::mpsc::Receiver<std::result::Result<AiEdit, String>>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
#[cfg(not(target_arch = "wasm32"))]
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}
#[cfg(not(target_arch = "wasm32"))]
impl Job {
    fn spawn(
        activity: &crate::activity::Activity,
        detail: &str,
        work: impl FnOnce(&std::sync::atomic::AtomicBool, &dyn Fn(Phase)) -> std::result::Result<AiEdit, String> + Send + 'static,
    ) -> std::result::Result<Self, String> {
        let (tx, reply) = std::sync::mpsc::channel();
        let (phase_tx, phases) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = cancel.clone();
        let task = activity.start("retouch", "AI Remove", crate::activity::Cancel::Flag(cancel.clone()));
        task.detail(detail);
        std::thread::Builder::new()
            .name("ai-remove".into())
            .spawn(move || {
                let result = crate::guard::catch("AI Remove", || {
                    work(&stopped, &|phase| {
                        let _ = phase_tx.send(phase);
                    })
                })
                .unwrap_or_else(Err);
                // A completion acknowledges that inference, process reaping and activity cleanup finished.
                drop(task);
                let _ = tx.send(result);
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { reply, phases, cancel })
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
    fn ai_remove_reconstruction_available(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.ai_models.snapshot["models"].as_array().is_some_and(|models| {
                models
                    .iter()
                    .any(|model| model["id"] == self.ai_models.snapshot["defaults"]["selected_model_id"] && model["supports_reconstruction"] == true)
            })
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    }
    fn ai_remove_tag(&self) -> Option<(PhotoId, std::sync::Arc<DevelopSettings>, u64)> {
        let id = self.active()?;
        let d = self.develop_of(id)?;
        Some((id, d, self.library_generation()))
    }
    pub fn ai_remove_poll(&mut self) {
        self.segment_ai_remove_poll();
        if self.ai_remove.selection_tag.is_some()
            && !self.ai_remove.selection_tag.as_ref().zip(self.ai_remove_tag()).is_some_and(|((id, settings, generation), (now, current, g))| {
                *id == now && *generation == g && std::sync::Arc::ptr_eq(settings, &current)
            })
        {
            self.ai_remove_clear_object();
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
            self.ai_remove.cancel_reason = Some("AI result discarded because the photo or edits changed.");
            self.ai_remove.message = self.ai_remove.cancel_reason.unwrap_or_default().into();
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(job) = &self.ai_remove.job {
            let cancelled = job.cancel.load(std::sync::atomic::Ordering::Relaxed);
            if cancelled {
                self.ai_remove.phase = Phase::Cancelling;
                self.ai_remove.draft = None;
                self.ai_remove.preview_settings = None;
                self.ai_remove.preview_original = false;
                self.ai_remove.tag = None;
                self.ai_remove.message = self.ai_remove.cancel_reason.unwrap_or("Cancelling AI Remove…").into();
            } else if let Some(phase) = job.phases.try_iter().last() {
                self.ai_remove.phase = phase;
                self.ai_remove.message = match phase {
                    Phase::Effects => "Stage 1/2: Finding shadows and reflections…".into(),
                    Phase::Reconstruction => "Stage 2/2: Reconstructing background details…".into(),
                    _ => self.ai_remove.message.clone(),
                };
            }
            let result = match job.reply.try_recv() {
                Ok(result) => Some(result),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err("AI worker stopped unexpectedly.".into())),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
            };
            if let Some(result) = result {
                let cancelled = job.cancel.load(std::sync::atomic::Ordering::Relaxed);
                self.ai_remove.job = None;
                if cancelled {
                    self.ai_remove.phase = Phase::Cancelled;
                    self.ai_remove.message = self.ai_remove.cancel_reason.unwrap_or("Cancelled.").into();
                } else {
                    match result {
                        Ok(edit) => {
                            self.ai_remove.draft = Some(edit);
                            self.ai_remove.phase = Phase::Ready;
                            self.ai_remove.message = if self.ai_remove.reconstruction_active {
                                "Result ready. Review shadows, texture and transitions before accepting."
                            } else if self.ai_remove.effects_active {
                                "Result ready. Shadows and reflections may extend beyond the object. Review before accepting."
                            } else {
                                "Result ready. Review the patch before accepting."
                            }
                            .into();
                        }
                        Err(e) => {
                            self.ai_remove.phase = Phase::Failed;
                            self.ai_remove.message = e;
                            self.ai_remove.tag = None;
                        }
                    }
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
        self.ai_remove.effects_active = false;
        self.ai_remove.reconstruction_active = false;
        self.ai_remove.selection_revision = self.ai_remove.selection_revision.wrapping_add(1);
        self.ai_remove.selecting = false;
        self.ai_remove.cancel_reason = None;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(job) = &self.ai_remove.job {
            job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.ai_remove.phase = if self.ai_remove.busy() { Phase::Cancelling } else { Phase::Cancelled };
        self.ai_remove.draft = None;
        self.ai_remove.preview_settings = None;
        self.ai_remove.preview_original = false;
        self.ai_remove.tag = None;
        self.ai_remove.message = if self.ai_remove.busy() { "Cancelling AI Remove…" } else { "Cancelled." }.into();
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn ai_remove_start(
        &mut self,
        backend: Option<&'static str>,
        model_id: Option<String>,
        include_effects: Option<bool>,
        reconstruct: Option<bool>,
    ) -> Result<Value> {
        self.ai_remove_poll();
        if self.ai_remove.busy() {
            return Err(error("AI Remove is already running."));
        }
        if self.ai_models.busy() {
            return Err(error("Wait for model management to finish."));
        }
        if self.ai_remove.selecting {
            return Err(error("Wait for the object selection."));
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
        let ai_root = crate::ai_models::root()?;
        let selected = crate::ai_models::selected(&ai_root, model_id.as_deref(), backend)?;
        if model_id.as_ref().is_some_and(|id| id != &selected.id) {
            return Err(error("The requested AI model is unavailable; no replacement was selected."));
        }
        let resolved = crate::ai_models::resolve(&ai_root, Some(&selected.id), None, false)?;
        if resolved.id != selected.id {
            return Err(error("Selected AI model identity changed."));
        }
        let capable = resolved.supports_effects();
        let effects = include_effects.unwrap_or(self.ai_remove.include_effects.unwrap_or(true)) && capable;
        if include_effects == Some(true) && !capable {
            return Err(error("Shadows and reflections need a supported trusted native ObjectClear runtime."));
        }
        let reconstruction = if effects && reconstruct.or(self.ai_remove.reconstruct) != Some(false) {
            match crate::ai_models::reconstruction_package(&ai_root, &resolved) {
                Ok(p) => Some((p.id, p.runtime_id)),
                Err(e) if reconstruct == Some(true) || self.ai_remove.reconstruct == Some(true) => return Err(e),
                Err(_) => None,
            }
        } else {
            if reconstruct == Some(true) {
                return Err(error("Two-stage reconstruction requires supported ObjectClear effects."));
            }
            None
        };
        let reconstruction_active = reconstruction.is_some();
        let selected_runtime_id = resolved.runtime_id.clone();
        let refinements = self.ai_remove.object_selection.as_ref().map(|o| o.refinements.clone()).unwrap_or_default();
        let backend = selected.backend.clone();
        let selected_id = selected.id.clone();
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
        let photo = self.catalog.photo(id).cloned().ok_or_else(|| error("Photo missing."))?;
        let source = self.media.source_ref(&photo, crate::media::SourceLevel::Full);
        let info = self.source_info(id);
        let settings = (*d).clone();
        static NEXT_SEED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(42);
        let seed = if backend == "lama" { 0 } else { next_diffusion_seed(&NEXT_SEED) };
        let detail = selected_id.clone();
        let job = Job::spawn(&self.activity, &detail, move |stopped, progress| {
            progress(Phase::Loading);
            let _store_lock = crate::ai_models::store_lock(&ai_root).map_err(|e| e.to_string())?;
            if stopped.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("Cancelled.".into());
            }
            let selected = crate::ai_models::resolve(&ai_root, Some(&selected_id), Some(&backend), false).map_err(|e| e.to_string())?;
            if selected.id != selected_id || selected.runtime_id != selected_runtime_id {
                return Err("Selected AI package identity changed.".into());
            }
            selected.verify_execution().map_err(|e| e.to_string())?;
            let reconstruction = if let Some((model_id, runtime_id)) = reconstruction {
                let second = crate::ai_models::reconstruction_package(&ai_root, &selected).map_err(|e| e.to_string())?;
                if second.id != model_id || second.runtime_id != runtime_id {
                    return Err("Reconstruction package identity changed.".into());
                }
                Some(second)
            } else {
                None
            };
            match selected.adapter().map_err(|e| e.to_string())?.execution {
                crate::ai_models::ExecutionKind::PythonProtocolV1 | crate::ai_models::ExecutionKind::NativeProtocolV1 => run_worker_with_progress(
                    &selected.runtime,
                    &backend,
                    Some(&selected),
                    reconstruction.as_ref(),
                    source,
                    info,
                    settings,
                    spot,
                    segmented,
                    refinements,
                    effects,
                    index.is_none(),
                    seed,
                    stopped,
                    progress,
                ),
            }
        })
        .map_err(error)?;
        self.ai_remove = AiRemove {
            include_effects: self.ai_remove.include_effects,
            reconstruct: self.ai_remove.reconstruct,
            reconstruction_active,
            effects_active: effects,
            object_selection: self.ai_remove.object_selection.clone(),
            selecting: false,
            selection_revision: self.ai_remove.selection_revision,
            phase: Phase::Loading,
            cancel_reason: None,
            message: "AI Remove is working locally…".into(),
            draft: None,
            preview_settings: None,
            preview_original: false,
            tag: self.ai_remove_tag(),
            spot: index,
            selection: self.ai_remove.selection.clone(),
            selection_tag: self.ai_remove.selection_tag.clone(),
            job: Some(job),
        };
        Ok(json!({"running":true}))
    }
}

fn reconstruction_param(p: &Value) -> Result<Option<bool>> {
    match p.get("reconstruction").or_else(|| p.get("mode")) {
        None => Ok(None),
        Some(Value::String(s)) if s == "objectclear-rorem" => Ok(Some(true)),
        Some(Value::String(s)) if s == "selected" => Ok(Some(false)),
        _ => Err(error("Invalid reconstruction mode; use objectclear-rorem or selected.")),
    }
}

pub(crate) fn specs() -> Vec<crate::cmd::CommandSpec> {
    use crate::cmd::{always, cmd, has_active};
    vec![
        cmd!(query "aiRemove.objectPoint", "Select AI Object", [], None, "{x,y,exclude?:bool} — refine a temporary object selection", has_active, |s, p| s.ai_remove_object_point(p)),
        cmd!(query "aiRemove.objectMargin", "Object Selection Margin", [], None, "{size:0..0.02} — visible margin as a fraction of the long edge", has_active, |s, p| s.ai_remove_object_margin(p)),
        cmd!(query "aiRemove.refineBrush", "Refine AI Selection", [], None, "{points:[[x,y],…],size?,subtract?:bool}", has_active, |s, p| s.ai_remove_refine_brush(p)),
        cmd!(query "aiRemove.select", "Paint AI Selection", [], None, "{points:[[x,y],…],size?,feather?,opacity?} — temporary AI mask; no photo edit", has_active, |s, p| {
            let selection = selection_param(p)?;
            let tag = s.ai_remove_tag().ok_or_else(|| error("Select a photo."))?;
            s.ai_remove_cancel();
            s.ai_remove_clear_object();
            s.ai_remove.selection = Some(selection);
            s.ai_remove.selection_tag = Some(tag);
            if !s.ai_remove.busy() { s.ai_remove.phase = Phase::Idle; }
            s.ai_remove.message = "AI selection ready. Run AI Remove to create a preview.".into();
            Ok(json!({"selected":true}))
        }),
        cmd!(query "aiRemove.clearSelection", "Clear AI Selection", [], None, "{} — discard the temporary AI mask and draft", always, |s, _| {
            s.ai_remove_cancel();
            s.ai_remove_clear_object();
            s.ai_remove.selection = None;
            s.ai_remove.selection_tag = None;
            Ok(json!({}))
        }),
        cmd!(query "aiRemove.reconstruction", "Two-stage Reconstruction", [], None, "{mode:objectclear-rorem|selected} — discard draft when changing reconstruction", always, |s, p| {
            s.ai_remove_poll();
            if s.ai_remove.busy() || s.ai_remove.selecting { return Err(error("Wait for AI Remove and object selection to finish.")); }
            let enabled = reconstruction_param(p)?.ok_or_else(|| error("Give reconstruction mode."))?;
            if s.ai_remove.reconstruct != Some(enabled) { s.ai_remove_cancel(); s.ai_remove.reconstruct = Some(enabled); }
            Ok(json!({"reconstruction": if enabled { "objectclear-rorem" } else { "selected" }}))
        }),
        cmd!(query "aiRemove.effects", "Include Shadows and Reflections", [], None, "{enabled:bool} — discard draft when changing effects preference", always, |s, p| {
            s.ai_remove_poll();
            if s.ai_remove.busy() { return Err(error("Wait for AI Remove to finish.")); }
            if s.ai_remove.selecting { return Err(error("Wait for the object selection.")); }
            let enabled = p.get("enabled").and_then(Value::as_bool).ok_or_else(|| error("Give enabled as a boolean."))?;
            if s.ai_remove.include_effects != Some(enabled) {
                s.ai_remove_cancel();
                s.ai_remove.include_effects = Some(enabled);
            }
            Ok(json!({"enabled": enabled}))
        }),
        cmd!(query "aiRemove.start", "AI Remove", [], None, "{modelId?:installed revision ID,backend?:objectclear|lama|rorem,includeEffects?:bool,reconstruction?:objectclear-rorem|selected} — starts a local background draft from the painted selection", has_active, |s, p| {
            let backend = if p.get("backend").is_some() { Some(backend_param(p)?) } else { None };
            let model_id = match p.get("modelId") { None=>None,Some(Value::String(id)) if !id.is_empty() && id.len()<=256=>Some(id.clone()),_=>return Err(error("Give modelId as a nonempty installed model ID.")) };
            let include_effects = match p.get("includeEffects") {
                Some(v) => Some(v.as_bool().ok_or_else(|| error("Give includeEffects as a boolean."))?),
                None => None,
            };
            let reconstruct = reconstruction_param(p)?;
            #[cfg(not(target_arch = "wasm32"))] { s.ai_remove_start(backend, model_id, include_effects, reconstruct) }
            #[cfg(target_arch = "wasm32")] { let _ = (s, backend, model_id, include_effects, reconstruct); Err(error("Local AI Remove is unavailable in the browser.")) }
        }),
        cmd!(query "aiRemove.status", "AI Remove Status", [], None, "{}", always, |s, _| { s.ai_remove_poll(); Ok(json!({"reconstruction":if s.ai_remove.reconstruction_active { "objectclear-rorem" } else { "selected" },"reconstructionPreference":match s.ai_remove.reconstruct { Some(true)=>"objectclear-rorem",Some(false)=>"selected",None=>"auto" },"stage":match s.ai_remove.phase { Phase::Effects=>1,Phase::Reconstruction=>2,_=>0 },"draftStages":s.ai_remove.draft.as_ref().map(|e|&e.stages),"reconstructionAvailable":s.ai_remove_reconstruction_available(),"includeEffects":s.ai_remove.include_effects.unwrap_or(true),"effectsActive":s.ai_remove.effects_active,"running":s.ai_remove.busy(),"ready":s.ai_remove.draft.is_some(),"selected":s.ai_remove.selection.is_some() || s.ai_remove.object_selection.as_ref().is_some_and(|o|o.seg.is_some()),"selecting":s.ai_remove.selecting,"previewOriginal":s.ai_remove.preview_original,"message":s.ai_remove.message,"phase":s.ai_remove.phase})) }),
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
            s.ai_remove_clear_object();
            s.ai_remove.selection = None;
            s.ai_remove.selection_tag = None;
            s.ai_remove.phase = Phase::Idle;
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
fn next_diffusion_seed(counter: &std::sync::atomic::AtomicU32) -> u32 {
    // Skip zero even when the counter wraps after u32::MAX.
    counter
        .try_update(std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed, |seed| Some(seed.wrapping_add(1).max(1)))
        .unwrap_or(42)
        .max(1)
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
#[cfg_attr(not(test), allow(dead_code))]
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
    run_worker_with_progress(runtime, backend, package, None, source, info, d, spot, None, Vec::new(), false, continuous, seed, cancel, &|_| {})
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn run_worker_with_progress(
    runtime: &std::path::Path,
    backend: &str,
    package: Option<&crate::ai_models::Resolved>,
    reconstruction: Option<&crate::ai_models::Resolved>,
    source: crate::media::SourceRef,
    info: lightcraft_pipeline::SourceInfo,
    d: DevelopSettings,
    spot: Spot,
    segmented: Option<lightcraft_develop::SegMask>,
    refinements: Vec<(Spot, bool)>,
    effects: bool,
    continuous: bool,
    seed: u32,
    cancel: &std::sync::atomic::AtomicBool,
    progress: &dyn Fn(Phase),
) -> std::result::Result<AiEdit, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1200);
    progress(Phase::Preparing);
    let mut prepared = match segmented {
        Some(seg) => input::prepare_segmented_protected(source, info, d, seg, &refinements, seed, cancel)?,
        None => input::prepare(source, info, d, spot, continuous, seed, cancel)?,
    };
    prepared.request.effects = effects;
    if let Some(second) = reconstruction {
        let first = package.ok_or("Two-stage reconstruction requires verified packages.")?;
        let stages = [
            first.stage_record(lightcraft_develop::ai_edit::AiStageRole::Effects, seed, 20).map_err(|e| e.to_string())?,
            second.stage_record(lightcraft_develop::ai_edit::AiStageRole::Reconstruction, seed, 20).map_err(|e| e.to_string())?,
        ];
        runner::execute_chain_with_progress(
            prepared,
            &protocol::ProtocolRunner { runtime, backend, package, deadline: Some(deadline) },
            &protocol::ProtocolRunner { runtime: &second.runtime, backend: &second.backend, package: Some(second), deadline: Some(deadline) },
            stages,
            cancel,
            deadline,
            progress,
        )
    } else {
        runner::execute_with_progress(prepared, &protocol::ProtocolRunner { runtime, backend, package, deadline: None }, cancel, progress)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod lifecycle_tests;

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn draft(s: &mut Session) {
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
                stages: Vec::new(),
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
cp "$(dirname "$4")/input.rgb" "$(dirname "$0")/../captured-input.rgb"
cp "$(dirname "$4")/mask.u8" "$(dirname "$0")/../captured-mask.u8"
cp "$(dirname "$0")/../pixels.rgba" "$(dirname "$5")/output.rgba"
printf '%s' '{"version":1,"width":128,"height":96,"model":"test","model_version":"geometry-1"}' > "$5"
"#,
        )
        .unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut settings = DevelopSettings::default();
        settings.optics.lens_profile = true;
        settings.optics.remove_ca = true;
        settings.geometry.vertical = 12.0;
        settings.geometry.horizontal = -8.0;
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
            runtime_id: "test-runtime".into(),
            execution: crate::ai_models::ExecutionKind::PythonProtocolV1,
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
        assert_ne!(edit.support.as_ref().unwrap().geometry.as_ref().unwrap().perspective, lightcraft_geom::Homography::IDENTITY);
        assert!(edit.support.as_ref().unwrap().geometry.as_ref().unwrap().lens.is_some());
        assert_eq!(request["version"], 1);
        assert_eq!(request["steps"], 20);
        assert_eq!(request["seed"], 0);
        assert_eq!(request["backend"], "lama");
        let expected_rgb: Vec<u8> = (0..128 * 96).flat_map(|_| [0.1f32, 0.2, 0.3].into_iter().flat_map(f32::to_le_bytes)).collect();
        assert_eq!(std::fs::read(runtime.path().join("captured-input.rgb")).unwrap(), expected_rgb);
        let mask = std::fs::read(runtime.path().join("captured-mask.u8")).unwrap();
        assert_eq!(mask.len(), 128 * 96);
        let pixels = edit.decode_pixels().unwrap();
        for (pixel, alpha) in pixels.iter().zip(mask) {
            assert_eq!(pixel[3], f32::from(alpha) / 255.0);
            if alpha == 0 {
                assert_eq!(*pixel, [0.1, 0.2, 0.3, 0.0]);
            }
        }
        let mut patched = settings.clone();
        patched.ai_edits.push(edit);
        let src = lightcraft_raster::Rgb32f::from_fn(128, 96, |_, _| [0.1, 0.2, 0.3]);
        let result = lightcraft_pipeline::ai_edits::source_with_edits(&src, &patched);
        assert!(result.data.iter().any(|p| p[0] > 0.5));
        assert_eq!(result.get(0, 0), src.get(0, 0));
        assert_eq!(settings, original);
    }
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn reconstruction_status_uses_selected_cached_model_capability() {
        let mut s = Session::with_demo();
        assert_eq!(s.execute("aiRemove.status", &json!({})).unwrap()["reconstructionAvailable"], false);
        s.ai_models.snapshot = json!({"defaults":{"selected_model_id":"capable"},"models":[{"id":"capable","supports_reconstruction":true},{"id":"other","supports_reconstruction":false}]});
        assert_eq!(s.execute("aiRemove.status", &json!({})).unwrap()["reconstructionAvailable"], true);
        s.ai_models.snapshot["defaults"]["selected_model_id"] = json!("other");
        assert_eq!(s.execute("aiRemove.status", &json!({})).unwrap()["reconstructionAvailable"], false);
    }
    #[test]
    #[cfg(target_arch = "wasm32")]
    fn reconstruction_status_is_unavailable_in_browser() {
        let mut s = Session::with_demo();
        assert_eq!(s.execute("aiRemove.status", &json!({})).unwrap()["reconstructionAvailable"], false);
    }
    #[test]
    fn reconstruction_controls_validate_preserve_selection_and_discard_only_draft() {
        let mut s = Session::with_demo();
        s.execute("aiRemove.select", &json!({"points":[[0.5,0.5]]})).unwrap();
        draft(&mut s);
        let selection = s.ai_remove.selection.clone();
        let settings = s.develop_of(s.active().unwrap()).unwrap();
        for mode in [json!("wrong"), json!(null), json!(42)] {
            assert!(s.execute("aiRemove.reconstruction", &json!({"mode":mode})).is_err());
            assert!(s.ai_remove.draft.is_some());
        }
        s.ai_remove.selecting = true;
        assert!(s.execute("aiRemove.reconstruction", &json!({"mode":"selected"})).is_err());
        assert!(s.ai_remove.selecting);
        s.ai_remove.selecting = false;
        s.execute("aiRemove.reconstruction", &json!({"mode":"selected"})).unwrap();
        assert!(s.ai_remove.draft.is_none());
        assert_eq!(s.ai_remove.selection, selection);
        assert_eq!(s.execute("aiRemove.status", &json!({})).unwrap()["reconstructionPreference"], "selected");
        s.execute("aiRemove.reconstruction", &json!({"mode":"objectclear-rorem"})).unwrap();
        assert_eq!(s.ai_remove.reconstruct, Some(true));
        assert!(std::sync::Arc::ptr_eq(&settings, &s.develop_of(s.active().unwrap()).unwrap()));
        assert!(s.execute("aiRemove.start", &json!({"reconstruction":"wrong"})).is_err());
    }
    #[test]
    fn effects_preference_is_transient_discards_draft_and_validates_boolean() {
        let mut s = Session::with_demo();
        draft(&mut s);
        let id = s.active().unwrap();
        let settings = s.develop_of(id).unwrap();
        assert_eq!(s.execute("aiRemove.status", &json!({})).unwrap()["includeEffects"], true);
        assert!(s.execute("aiRemove.effects", &json!({"enabled":"yes"})).is_err());
        assert!(s.ai_remove.draft.is_some());
        s.ai_remove.selecting = true;
        assert!(s.execute("aiRemove.effects", &json!({"enabled":false})).unwrap_err().to_string().contains("object selection"));
        assert!(s.ai_remove.draft.is_some());
        assert!(s.ai_remove.selecting);
        s.ai_remove.selecting = false;
        s.execute("aiRemove.effects", &json!({"enabled":false})).unwrap();
        assert!(s.ai_remove.draft.is_none());
        assert_eq!(s.execute("aiRemove.status", &json!({})).unwrap()["includeEffects"], false);
        assert!(std::sync::Arc::ptr_eq(&settings, &s.develop_of(id).unwrap()));
        assert!(s.execute("aiRemove.start", &json!({"includeEffects":42})).is_err());
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
