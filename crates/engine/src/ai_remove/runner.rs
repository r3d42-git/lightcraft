//! Injectable inference boundary: scene-linear Rec.2020 RGB and binary mask only.
//! Effects coverage is accepted only for an explicitly authorized invocation.
use super::{input::PreparedInput, patch};
#[cfg(test)]
use lightcraft_develop::AiEdit;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RetouchIdentity {
    pub model: String,
    pub runtime: PathBuf,
    pub adapter: &'static str,
}

#[derive(Clone)]
pub(super) struct RetouchInput {
    pub width: usize,
    pub height: usize,
    /// As-shot scene-linear Rec.2020; neither display encoded nor premultiplied.
    pub rgb: Vec<[f32; 3]>,
    pub mask: Vec<u8>,
    pub rect: [f64; 4],
    pub seed: u32,
    pub steps: u32,
    pub effects: bool,
    pub protected: Vec<bool>,
}

/// Frozen resolved execution identity travels with each invocation, without changing registry v1.
pub(super) struct RetouchRequest<'a> {
    pub input: &'a RetouchInput,
    pub identity: &'a RetouchIdentity,
}

impl RetouchInput {
    fn validate(&self) -> Result<(), String> {
        let count = self
            .width
            .checked_mul(self.height)
            .filter(|n| *n > 0 && self.width <= 1024 && self.height <= 1024)
            .ok_or("Invalid AI inference dimensions.")?;
        if self.rgb.len() != count || self.mask.len() != count || !self.mask.iter().all(|v| *v == 0 || *v == 255) {
            return Err("Invalid AI inference input or binary mask.".into());
        }
        if self.protected.len() != count {
            return Err("Invalid AI protection mask.".into());
        }
        Ok(())
    }
}

pub(super) struct RetouchProvenance {
    pub model: String,
    pub revision: String,
    pub identity: RetouchIdentity,
}

pub(super) struct RetouchOutput {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<[f32; 3]>,
    pub provenance: RetouchProvenance,
    pub coverage: Option<Vec<u8>>,
}
impl RetouchOutput {
    pub fn validate(&self, input: &RetouchInput) -> Result<(), String> {
        if self.width != input.width || self.height != input.height || self.rgb.len() != input.rgb.len() {
            return Err("AI worker returned mismatched dimensions.".into());
        }
        match (&self.coverage, input.effects) {
            (Some(coverage), true)
                if coverage.len() == input.mask.len()
                    && coverage.iter().zip(&input.mask).all(|(a, mask)| (*a == 0 || *a == 255) && (*mask == 0 || *a == 255)) => {}
            (None, false) => {}
            _ => return Err("AI worker returned invalid or unrequested effects coverage.".into()),
        }
        if self.provenance.model.trim().is_empty() || self.provenance.revision.trim().is_empty() {
            return Err("Worker omitted model provenance or revision.".into());
        }
        if !self.rgb.iter().flatten().all(|v| v.is_finite()) {
            return Err("AI worker returned non-finite pixel data.".into());
        }
        Ok(())
    }
}

pub(super) trait RetouchRunner {
    /// Captured from the resolved immutable package, never inferred from output text.
    fn identity(&self) -> RetouchIdentity;
    fn run(&self, request: RetouchRequest<'_>, cancel: &AtomicBool) -> Result<RetouchOutput, String>;
}

#[cfg(test)]
pub(super) fn execute(prepared: PreparedInput, runner: &dyn RetouchRunner, cancel: &AtomicBool) -> Result<AiEdit, String> {
    execute_with_progress(prepared, runner, cancel, &|_| {})
}

#[cfg(test)]
pub(super) fn execute_with_progress(
    prepared: PreparedInput,
    runner: &dyn RetouchRunner,
    cancel: &AtomicBool,
    progress: &dyn Fn(super::Phase),
) -> Result<AiEdit, String> {
    progress(super::Phase::Running);
    let output = invoke_checked(&prepared.request, runner, cancel)?;
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    progress(super::Phase::Finalizing);
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    patch::construct(prepared, output)
}

/// Keep both local drafts from the same inference; toggling never reruns a model.
pub(super) fn execute_variants_with_progress(
    prepared: PreparedInput,
    runner: &dyn RetouchRunner,
    cancel: &AtomicBool,
    deadline: std::time::Instant,
    progress: &dyn Fn(super::Phase),
) -> Result<super::LightingDrafts, String> {
    progress(super::Phase::Running);
    let output = invoke_checked(&prepared.request, runner, cancel)?;
    progress(super::Phase::Finalizing);
    construct_local_variants(prepared, output, Vec::new(), cancel, deadline)
}

fn construct_local_variants(
    prepared: PreparedInput,
    output: RetouchOutput,
    stages: Vec<lightcraft_develop::ai_edit::AiEditStage>,
    cancel: &AtomicBool,
    deadline: std::time::Instant,
) -> Result<super::LightingDrafts, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    output.validate(&prepared.request)?;
    let mut matching = prepared.request.clone();
    if let Some(coverage) = &output.coverage {
        matching.mask = coverage.clone();
    }
    for (mask, protected) in matching.mask.iter_mut().zip(&matching.protected) {
        if *protected {
            *mask = 0;
        }
    }
    matching.effects = false;
    // Scale the correction bound for HDR without using masked original RGB as
    // a photometric reference. Local workers already restore original context.
    let gain = output.rgb.iter().flatten().fold(1.0f32, |gain, value| gain.max(value.abs()));
    let matched = super::lighting::apply_local(&matching, &output.rgb, gain, cancel, deadline).and_then(|rgb| {
        let suffix = "; lighting=local-frequency-v2";
        let model = format!("{}{suffix}", output.provenance.model);
        if model.len() > 256 {
            return Err("Lighting provenance is too long. Turn off Match lighting to use the unadjusted result.".into());
        }
        let matched = RetouchOutput {
            width: output.width,
            height: output.height,
            rgb,
            coverage: output.coverage.clone(),
            provenance: RetouchProvenance { model, revision: output.provenance.revision.clone(), identity: output.provenance.identity.clone() },
        };
        let mut edit = patch::construct(PreparedInput { request: prepared.request.clone(), support: prepared.support.clone() }, matched)?;
        edit.stages = stages.clone();
        edit.pixel_count().map_err(|e| e.to_string())?;
        Ok(edit)
    });
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    let mut raw = patch::construct(prepared, output)?;
    raw.stages = stages;
    raw.pixel_count().map_err(|e| e.to_string())?;
    Ok(super::LightingDrafts { raw, matched })
}

pub(super) fn invoke_checked(input: &RetouchInput, runner: &dyn RetouchRunner, cancel: &AtomicBool) -> Result<RetouchOutput, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    input.validate()?;
    let identity = runner.identity();
    let output = runner.run(RetouchRequest { input, identity: &identity }, cancel)?;
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    if output.provenance.identity != identity {
        return Err("AI worker returned mismatched package provenance.".into());
    }
    output.validate(input)?;
    Ok(output)
}

#[cfg(test)]
pub(super) fn execute_chain_with_progress(
    prepared: PreparedInput,
    first: &dyn RetouchRunner,
    second: &dyn RetouchRunner,
    stages: [lightcraft_develop::ai_edit::AiEditStage; 2],
    cancel: &AtomicBool,
    deadline: std::time::Instant,
    progress: &dyn Fn(super::Phase),
) -> Result<AiEdit, String> {
    execute_chain_result_with_progress(prepared, first, second, stages, cancel, deadline, progress, |prepared, output, stages| {
        let mut edit = patch::construct(prepared, output)?;
        edit.stages = stages;
        edit.pixel_count().map_err(|e| e.to_string())?;
        Ok(edit)
    })
}

pub(super) fn execute_chain_variants_with_progress(
    prepared: PreparedInput,
    first: &dyn RetouchRunner,
    second: &dyn RetouchRunner,
    stages: [lightcraft_develop::ai_edit::AiEditStage; 2],
    cancel: &AtomicBool,
    deadline: std::time::Instant,
    progress: &dyn Fn(super::Phase),
) -> Result<super::LightingDrafts, String> {
    execute_chain_result_with_progress(prepared, first, second, stages, cancel, deadline, progress, |prepared, output, stages| {
        construct_local_variants(prepared, output, stages, cancel, deadline)
    })
}

#[allow(clippy::too_many_arguments)]
fn execute_chain_result_with_progress<T>(
    prepared: PreparedInput,
    first: &dyn RetouchRunner,
    second: &dyn RetouchRunner,
    mut stages: [lightcraft_develop::ai_edit::AiEditStage; 2],
    cancel: &AtomicBool,
    deadline: std::time::Instant,
    progress: &dyn Fn(super::Phase),
    finish: impl FnOnce(PreparedInput, RetouchOutput, Vec<lightcraft_develop::ai_edit::AiEditStage>) -> Result<T, String>,
) -> Result<T, String> {
    use sha2::{Digest, Sha256};
    let budget = || if std::time::Instant::now() >= deadline { Err("Two-stage AI Remove timed out.".to_string()) } else { Ok(()) };
    if !prepared.request.effects {
        return Err("Two-stage reconstruction requires object effects.".into());
    }
    budget()?;
    progress(super::Phase::Effects);
    let mut effects = invoke_checked(&prepared.request, first, cancel).map_err(|e| format!("Effects stage 1/2: {e}"))?;
    if effects.provenance.revision != stages[0].model_revision || effects.provenance.identity.model != stages[0].model_id {
        return Err("Effects stage 1/2: mismatched trusted model provenance.".into());
    }
    let coverage = effects.coverage.take().ok_or("Effects stage 1/2: missing coverage.")?;
    drop(effects); // Its RGB never enters reconstruction and no intermediate patch exists.
    let mut reconstruction = prepared.request.clone();
    reconstruction.mask = coverage.iter().zip(&prepared.request.protected).map(|(v, p)| if *p { 0 } else { *v }).collect();
    reconstruction.effects = false;
    if !reconstruction.mask.contains(&255) {
        return Err("No unprotected pixels to reconstruct.".into());
    }
    stages[0].mask_sha256 = format!("{:x}", Sha256::digest(&coverage));
    stages[1].mask_sha256 = format!("{:x}", Sha256::digest(&reconstruction.mask));
    budget()?;
    progress(super::Phase::Reconstruction);
    let mut output = invoke_checked(&reconstruction, second, cancel).map_err(|e| format!("Reconstruction stage 2/2: {e}"))?;
    if output.provenance.revision != stages[1].model_revision || output.provenance.identity.model != stages[1].model_id {
        return Err("Reconstruction stage 2/2: mismatched trusted model provenance.".into());
    }
    budget()?;
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    // C retains superset validation; protected cells are independently removed by patch construction.
    output.coverage = Some(coverage);
    progress(super::Phase::Finalizing);
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    finish(prepared, output, stages.into())
}

#[cfg(test)]
#[path = "chain_tests.rs"]
mod chain_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_develop::{DevelopSettings, Spot, ai_edit::AiBrushSupport};
    struct SyntheticRunner;
    impl RetouchRunner for SyntheticRunner {
        fn identity(&self) -> RetouchIdentity {
            RetouchIdentity { model: "synthetic-1".into(), runtime: "synthetic".into(), adapter: "test" }
        }
        fn run(&self, request: RetouchRequest<'_>, _: &AtomicBool) -> Result<RetouchOutput, String> {
            let input = request.input;
            Ok(RetouchOutput {
                width: input.width,
                height: input.height,
                rgb: vec![[0.75, 0.5, 0.25]; input.rgb.len()],
                coverage: None,
                provenance: RetouchProvenance { model: "synthetic".into(), revision: "1".into(), identity: request.identity.clone() },
            })
        }
    }
    fn prepared() -> PreparedInput {
        PreparedInput {
            request: RetouchInput {
                width: 2,
                height: 1,
                rgb: vec![[0.125, 0.25, 0.5]; 2],
                mask: vec![0, 255],
                rect: [0.0, 0.0, 1.0, 1.0],
                seed: 42,
                steps: 20,
                effects: false,
                protected: vec![false; 2],
            },
            support: Some(AiBrushSupport { geometry: None, continuous: true, points: vec![[0.75, 0.5]], radius: 0.1, feather: 0.0 }),
        }
    }
    #[test]
    fn injectable_runner_preserves_exact_engine_patch_authorization_and_metadata() {
        let input = prepared();
        let expected = AiEdit::from_pixels(
            AiEdit {
                id: 0,
                enabled: true,
                rect: input.request.rect,
                support: input.support.clone(),
                width: 2,
                height: 1,
                pixels: String::new(),
                model: "synthetic".into(),
                model_version: "1".into(),
                seed: 42,
                stages: Vec::new(),
            },
            &[[0.125, 0.25, 0.5, 0.0], [0.75, 0.5, 0.25, 1.0]],
        )
        .unwrap();
        let actual = execute(input, &SyntheticRunner, &AtomicBool::new(false)).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual.decode_pixels().unwrap()[0], [0.125, 0.25, 0.5, 0.0]);
    }
    #[test]
    fn effects_coverage_requires_opt_in_contains_selection_and_respects_protection() {
        let mut input = prepared();
        let mut output =
            SyntheticRunner.run(RetouchRequest { input: &input.request, identity: &SyntheticRunner.identity() }, &AtomicBool::new(false)).unwrap();
        output.coverage = Some(vec![255, 255]);
        assert!(output.validate(&input.request).is_err(), "unrequested model alpha never authorizes effects");
        input.request.effects = true;
        output.coverage = None;
        assert!(output.validate(&input.request).is_err(), "effects cannot fall back silently to object-only");
        for invalid in [vec![255], vec![255, 0], vec![128, 255]] {
            output.coverage = Some(invalid);
            assert!(output.validate(&input.request).is_err());
        }
        output.coverage = Some(vec![255, 255]);
        input.request.protected[0] = true;
        let edit = patch::construct(input, output).unwrap();
        assert_eq!(edit.decode_pixels().unwrap(), vec![[0.125, 0.25, 0.5, 0.0], [0.75, 0.5, 0.25, 1.0]]);
    }
    #[test]
    fn effects_patch_keeps_fused_replacement_rgb_without_second_attention_blend() {
        let mut input = prepared();
        input.request.effects = true;
        let mut output =
            SyntheticRunner.run(RetouchRequest { input: &input.request, identity: &SyntheticRunner.identity() }, &AtomicBool::new(false)).unwrap();
        output.coverage = Some(vec![255, 255]);
        let edit = patch::construct(input, output).unwrap();
        assert_eq!(edit.decode_pixels().unwrap(), vec![[0.75, 0.5, 0.25, 1.0]; 2]);
        assert!(edit.support.is_none(), "brush support must not clip extended effects");
    }
    #[test]
    fn mismatched_execution_identity_cannot_authorize_any_patch() {
        struct WrongIdentity;
        impl RetouchRunner for WrongIdentity {
            fn identity(&self) -> RetouchIdentity {
                SyntheticRunner.identity()
            }
            fn run(&self, request: RetouchRequest<'_>, cancel: &AtomicBool) -> Result<RetouchOutput, String> {
                let mut output = SyntheticRunner.run(request, cancel)?;
                output.provenance.identity.runtime = "another-runtime".into();
                Ok(output)
            }
        }
        assert!(execute(prepared(), &WrongIdentity, &AtomicBool::new(false)).unwrap_err().contains("package provenance"));
    }
    #[test]
    fn invalid_runner_dimensions_and_nonfinite_rgb_cannot_construct_patch() {
        let input = prepared();
        let mut output =
            SyntheticRunner.run(RetouchRequest { input: &input.request, identity: &SyntheticRunner.identity() }, &AtomicBool::new(false)).unwrap();
        output.width = 3;
        assert!(output.validate(&input.request).is_err());
        output.width = 2;
        output.rgb.pop();
        assert!(output.validate(&input.request).is_err());
        output.rgb.push([0.0; 3]);
        output.rgb[0][0] = f32::NAN;
        assert!(patch::construct(input, output).is_err());
        assert!(execute(prepared(), &SyntheticRunner, &AtomicBool::new(true)).unwrap_err().contains("Cancelled"));
    }
    #[test]
    fn malformed_inputs_and_empty_provenance_are_rejected() {
        let mut input = prepared();
        input.request.mask.pop();
        assert!(execute(input, &SyntheticRunner, &AtomicBool::new(false)).is_err());
        let mut input = prepared();
        input.request.mask[1] = 128;
        assert!(execute(input, &SyntheticRunner, &AtomicBool::new(false)).is_err());
        let input = prepared();
        let mut output =
            SyntheticRunner.run(RetouchRequest { input: &input.request, identity: &SyntheticRunner.identity() }, &AtomicBool::new(false)).unwrap();
        output.provenance.revision = " ".into();
        assert!(output.validate(&input.request).is_err());
    }
    #[test]
    fn late_cancel_does_not_construct_a_patch() {
        struct Late;
        impl RetouchRunner for Late {
            fn identity(&self) -> RetouchIdentity {
                SyntheticRunner.identity()
            }
            fn run(&self, request: RetouchRequest<'_>, cancel: &AtomicBool) -> Result<RetouchOutput, String> {
                let output = SyntheticRunner.run(request, cancel)?;
                cancel.store(true, Ordering::Relaxed);
                Ok(output)
            }
        }
        assert!(execute(prepared(), &Late, &AtomicBool::new(false)).unwrap_err().contains("Cancelled"));
    }
    #[test]
    fn preparation_uses_existing_patches_and_keeps_crop_out_of_source_space() {
        let mut settings = DevelopSettings { orientation: lightcraft_geom::Orientation::Rotate90, ..Default::default() };
        settings.crop.geometry.rect = lightcraft_geom::Rect::new(0.2, 0.2, 0.6, 0.6);
        settings.ai_edits.push(
            AiEdit::from_pixels(
                AiEdit {
                    id: 1,
                    enabled: true,
                    rect: [0.0, 0.0, 1.0, 1.0],
                    support: None,
                    width: 1,
                    height: 1,
                    pixels: String::new(),
                    model: "old".into(),
                    model_version: "1".into(),
                    seed: 1,
                    stages: Vec::new(),
                },
                &[[0.5, 0.25, 0.125, 1.0]],
            )
            .unwrap(),
        );
        let source = crate::media::SourceRef::Loaded(Box::new(crate::media::DecodedSource::new(
            std::sync::Arc::new(lightcraft_raster::Rgb32f::from_fn(16, 12, |_, _| [0.0; 3])),
            None,
        )));
        let prepared = super::super::input::prepare(
            source,
            Default::default(),
            settings,
            Spot { points: vec![lightcraft_geom::Point::new(0.5, 0.5)], size: 0.1, ..Default::default() },
            true,
            7,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(prepared.request.rect, [0.0, 0.0, 1.0, 1.0]);
        assert!(prepared.request.rgb.iter().all(|p| *p == [0.5, 0.25, 0.125]));
        assert_eq!(prepared.request.seed, 7);
        let geometry = prepared.support.as_ref().unwrap().geometry.as_ref().unwrap();
        assert_eq!(geometry.orientation, lightcraft_geom::Orientation::Rotate90);
        assert_eq!(geometry.source_size, [16, 12]);
    }
}

#[cfg(test)]
mod lighting_tests {
    use super::*;
    use crate::Session;
    use std::{
        sync::atomic::AtomicUsize,
        time::{Duration, Instant},
    };
    struct Local<'a>(&'a AtomicUsize);
    impl RetouchRunner for Local<'_> {
        fn identity(&self) -> RetouchIdentity {
            RetouchIdentity { model: "synthetic-local".into(), runtime: "fake".into(), adapter: "test" }
        }
        fn run(&self, request: RetouchRequest<'_>, _: &AtomicBool) -> Result<RetouchOutput, String> {
            self.0.fetch_add(1, Ordering::Relaxed);
            let input = request.input;
            Ok(RetouchOutput {
                width: input.width,
                height: input.height,
                rgb: input
                    .mask
                    .iter()
                    .zip(&input.protected)
                    .map(|(mask, protected)| [if *mask == 255 && !protected { 0.45 } else { 0.4 }; 3])
                    .collect(),
                coverage: None,
                provenance: RetouchProvenance { model: "synthetic-local".into(), revision: "local-1".into(), identity: request.identity.clone() },
            })
        }
    }
    fn prepared() -> PreparedInput {
        let width = 96;
        let height = 80;
        let mask: Vec<_> =
            (0..width * height).map(|i| if (24..72).contains(&(i % width)) && (20..60).contains(&(i / width)) { 255 } else { 0 }).collect();
        PreparedInput {
            request: RetouchInput {
                width,
                height,
                rgb: mask.iter().map(|m| if *m == 255 { [1.0, 0.1, 0.8] } else { [0.4; 3] }).collect(),
                mask,
                protected: vec![false; width * height],
                rect: [0., 0., 1., 1.],
                seed: 42,
                steps: 20,
                effects: false,
            },
            support: None,
        }
    }
    fn complete(variants: super::super::LightingDrafts) -> Session {
        let mut session = Session::with_demo();
        let (tx, reply) = std::sync::mpsc::channel();
        let (_, phases) = std::sync::mpsc::channel();
        session.ai_remove.tag = session.ai_remove_tag();
        session.ai_remove.job = Some(super::super::Job { reply, phases, cancel: std::sync::Arc::new(AtomicBool::new(false)) });
        tx.send(Ok(super::super::JobResult::Variants(Box::new(variants)))).unwrap();
        session.ai_remove_poll();
        session
    }
    #[test]
    fn local_pair_preserves_alpha_uses_one_inference_and_toggles_without_editing_saved_photo() {
        let calls = AtomicUsize::new(0);
        let input = prepared();
        let variants =
            execute_variants_with_progress(input, &Local(&calls), &AtomicBool::new(false), Instant::now() + Duration::from_secs(30), &|_| {})
                .unwrap();
        let raw = variants.raw.clone();
        let matched = variants.matched.as_ref().unwrap().clone();
        assert!(matched.model.ends_with("; lighting=local-frequency-v2"));
        assert_eq!(matched.model_version, raw.model_version);
        for (raw, matched) in raw.decode_pixels().unwrap().iter().zip(matched.decode_pixels().unwrap()) {
            assert_eq!(raw[3], matched[3]);
            if raw[3] == 0. {
                assert_eq!(*raw, matched);
            } else {
                assert!((matched[0] - 0.4).abs() < 1e-5);
            }
        }
        let mut session = complete(variants);
        assert!(!session.ai_remove.use_cloud);
        let id = session.active().unwrap();
        let before = session.develop_of(id).unwrap();
        assert_eq!(session.ai_remove.draft.as_ref(), Some(&matched));
        session.ai_remove_preview_settings().unwrap();
        session.execute("aiRemove.matchLighting", &serde_json::json!({"enabled":false})).unwrap();
        assert_eq!(session.ai_remove.draft.as_ref(), Some(&raw));
        assert!(session.ai_remove.preview_settings.is_none());
        session.execute("aiRemove.matchLighting", &serde_json::json!({"enabled":true})).unwrap();
        assert_eq!(session.ai_remove.draft.as_ref(), Some(&matched));
        assert_eq!(*session.develop_of(id).unwrap(), *before);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        session.execute("aiRemove.accept", &serde_json::json!({})).unwrap();
        let saved = session.develop_of(id).unwrap();
        assert_eq!(saved.ai_edits.last().unwrap().pixels, matched.pixels);
        session.execute("aiRemove.matchLighting", &serde_json::json!({"enabled":false})).unwrap();
        assert_eq!(*session.develop_of(id).unwrap(), *saved);
        assert!(session.ai_remove.lighting_drafts.is_none());
    }
    #[test]
    fn unsupported_local_matching_keeps_the_exact_unadjusted_result_available() {
        let calls = AtomicUsize::new(0);
        let mut input = prepared();
        input.request.mask.fill(255);
        let variants =
            execute_variants_with_progress(input, &Local(&calls), &AtomicBool::new(false), Instant::now() + Duration::from_secs(30), &|_| {})
                .unwrap();
        assert!(variants.matched.is_err());
        let raw = variants.raw.clone();
        let mut session = complete(variants);
        assert_eq!(session.ai_remove.phase, super::super::Phase::Failed);
        assert!(session.ai_remove.draft.is_none());
        session.execute("aiRemove.matchLighting", &serde_json::json!({"enabled":false})).unwrap();
        assert_eq!(session.ai_remove.draft.as_ref(), Some(&raw));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(session.execute("aiRemove.matchLighting", &serde_json::json!({"enabled":true})).is_err());
        assert_eq!(session.ai_remove.match_lighting, Some(false));
        assert_eq!(session.ai_remove.draft.as_ref(), Some(&raw));
    }
}

#[cfg(test)]
mod chain_lighting_persistence {
    use super::*;
    use lightcraft_develop::ai_edit::{AiEditStage, AiEffectsContract, AiStageRole};
    use std::{
        sync::atomic::AtomicUsize,
        time::{Duration, Instant},
    };
    struct Model<'a> {
        effects: bool,
        calls: &'a AtomicUsize,
    }
    impl RetouchRunner for Model<'_> {
        fn identity(&self) -> RetouchIdentity {
            RetouchIdentity {
                model: if self.effects { "effects-id" } else { "reconstruction-id" }.into(),
                runtime: "fixture".into(),
                adapter: "test",
            }
        }
        fn run(&self, r: RetouchRequest<'_>, _: &AtomicBool) -> Result<RetouchOutput, String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let rgb = if self.effects {
                vec![[100.; 3]; r.input.rgb.len()]
            } else {
                r.input.mask.iter().map(|m| [if *m == 255 { 0.45 } else { 0.4 }; 3]).collect()
            };
            Ok(RetouchOutput {
                width: r.input.width,
                height: r.input.height,
                rgb,
                coverage: self.effects.then(|| r.input.mask.clone()),
                provenance: RetouchProvenance {
                    model: if self.effects { "ObjectClear" } else { "RORem" }.into(),
                    revision: if self.effects { "effects-revision" } else { "reconstruction-revision" }.into(),
                    identity: r.identity.clone(),
                },
            })
        }
    }
    #[test]
    fn local_chain_matching_preserves_exact_model_revision_and_reopens_valid_stages() {
        let (width, height) = (96, 80);
        let count = width * height;
        let prepared = PreparedInput {
            request: RetouchInput {
                width,
                height,
                rgb: vec![[0.4; 3]; count],
                mask: (0..count).map(|i| if (24..72).contains(&(i % width)) && (20..60).contains(&(i / width)) { 255 } else { 0 }).collect(),
                protected: vec![false; count],
                rect: [0., 0., 1., 1.],
                seed: 42,
                steps: 20,
                effects: true,
            },
            support: None,
        };
        let effects_calls = AtomicUsize::new(0);
        let reconstruction_calls = AtomicUsize::new(0);
        let first = Model { effects: true, calls: &effects_calls };
        let second = Model { effects: false, calls: &reconstruction_calls };
        let stage = |effects| AiEditStage {
            role: if effects { AiStageRole::Effects } else { AiStageRole::Reconstruction },
            backend: if effects { "objectclear" } else { "rorem" }.into(),
            model_id: if effects { "effects-id" } else { "reconstruction-id" }.into(),
            model_revision: if effects { "effects-revision" } else { "reconstruction-revision" }.into(),
            runtime_id: "runtime-fixture".into(),
            binary_sha256: "a".repeat(64),
            model_manifest_sha256: "b".repeat(64),
            mask_sha256: "c".repeat(64),
            seed: 42,
            steps: 20,
            effects: effects.then_some(AiEffectsContract::ObjectEffectsV1),
        };
        let variants = execute_chain_variants_with_progress(
            prepared,
            &first,
            &second,
            [stage(true), stage(false)],
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(30),
            &|_| {},
        )
        .unwrap();
        let matched = variants.matched.unwrap();
        assert_eq!(matched.model_version, "reconstruction-revision");
        assert_eq!(matched.stages, variants.raw.stages);
        assert_eq!(matched.stages[1].model_revision, matched.model_version);
        assert!(matched.model.contains("lighting=local-frequency-v2"));
        let settings = lightcraft_develop::DevelopSettings { ai_edits: vec![matched.clone()], ..Default::default() };
        let saved = serde_json::to_vec(&settings).unwrap();
        let reopened: lightcraft_develop::DevelopSettings = serde_json::from_slice(&saved).unwrap();
        assert_eq!(reopened, settings);
        assert_eq!(reopened.ai_edits[0].decode_pixels().unwrap(), matched.decode_pixels().unwrap());
        assert_eq!(effects_calls.load(Ordering::Relaxed), 1);
        assert_eq!(reconstruction_calls.load(Ordering::Relaxed), 1);
    }
}
