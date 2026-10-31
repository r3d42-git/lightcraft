//! Injectable inference boundary: scene-linear Rec.2020 RGB and binary mask only.
//! Geometry and persisted alpha are intentionally absent from runner output.
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

pub(super) struct RetouchInput {
    pub width: usize,
    pub height: usize,
    /// As-shot scene-linear Rec.2020; neither display encoded nor premultiplied.
    pub rgb: Vec<[f32; 3]>,
    pub mask: Vec<u8>,
    pub rect: [f64; 4],
    pub seed: u32,
    pub steps: u32,
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
}
impl RetouchOutput {
    pub fn validate(&self, input: &RetouchInput) -> Result<(), String> {
        if self.width != input.width || self.height != input.height || self.rgb.len() != input.rgb.len() {
            return Err("AI worker returned mismatched dimensions.".into());
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
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    prepared.request.validate()?;
    let identity = runner.identity();
    progress(super::Phase::Running);
    let output = runner.run(RetouchRequest { input: &prepared.request, identity: &identity }, cancel)?;
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    if output.provenance.identity != identity {
        return Err("AI worker returned mismatched package provenance.".into());
    }
    progress(super::Phase::Finalizing);
    patch::construct(prepared, output)
}

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
            },
            support: AiBrushSupport { geometry: None, continuous: true, points: vec![[0.75, 0.5]], radius: 0.1, feather: 0.0 },
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
                support: Some(input.support.clone()),
                width: 2,
                height: 1,
                pixels: String::new(),
                model: "synthetic".into(),
                model_version: "1".into(),
                seed: 42,
            },
            &[[0.125, 0.25, 0.5, 0.0], [0.75, 0.5, 0.25, 1.0]],
        )
        .unwrap();
        let actual = execute(input, &SyntheticRunner, &AtomicBool::new(false)).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual.decode_pixels().unwrap()[0], [0.125, 0.25, 0.5, 0.0]);
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
        let geometry = prepared.support.geometry.as_ref().unwrap();
        assert_eq!(geometry.orientation, lightcraft_geom::Orientation::Rotate90);
        assert_eq!(geometry.source_size, [16, 12]);
    }
}
