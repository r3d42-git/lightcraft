//! Injectable inference boundary: scene-linear Rec.2020 RGB and binary mask only.
//! Effects coverage is accepted only for an explicitly authorized invocation.
use super::{input::PreparedInput, patch};
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

fn invoke_checked(input: &RetouchInput, runner: &dyn RetouchRunner, cancel: &AtomicBool) -> Result<RetouchOutput, String> {
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

pub(super) fn execute_chain_with_progress(
    prepared: PreparedInput,
    first: &dyn RetouchRunner,
    second: &dyn RetouchRunner,
    mut stages: [lightcraft_develop::ai_edit::AiEditStage; 2],
    cancel: &AtomicBool,
    deadline: std::time::Instant,
    progress: &dyn Fn(super::Phase),
) -> Result<AiEdit, String> {
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
    let mut edit = patch::construct(prepared, output)?;
    edit.stages = stages.into();
    edit.pixel_count().map_err(|e| e.to_string())?;
    Ok(edit)
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
