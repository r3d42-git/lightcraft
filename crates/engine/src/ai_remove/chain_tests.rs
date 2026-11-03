//! Dataflow and atomicity tests: no model or subprocess is needed to detect wrong orchestration.
use super::*;
use lightcraft_develop::ai_edit::{AiEditStage, AiEffectsContract, AiStageRole};
use std::{
    cell::Cell,
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
enum Outcome {
    Valid,
    Fail,
    BadCoverage,
    WrongIdentity,
    WrongRevision,
    Nonfinite,
    Cancel,
}
struct Fake {
    effects: bool,
    outcome: Outcome,
    calls: Cell<usize>,
}
impl Fake {
    fn new(effects: bool, outcome: Outcome) -> Self {
        Self { effects, outcome, calls: Cell::new(0) }
    }
}
impl RetouchRunner for Fake {
    fn identity(&self) -> RetouchIdentity {
        RetouchIdentity {
            model: if self.effects { "first-model" } else { "second-model" }.into(),
            runtime: if self.effects { "first" } else { "second" }.into(),
            adapter: "test",
        }
    }
    fn run(&self, r: RetouchRequest<'_>, cancel: &AtomicBool) -> Result<RetouchOutput, String> {
        self.calls.set(self.calls.get() + 1);
        assert_eq!(r.input.rgb, vec![[0.125, 0.25, 0.5]; 4], "RORem must receive original, never first-stage RGB");
        assert_eq!(r.input.rect, [0., 0., 1., 1.]);
        assert_eq!(r.input.seed, 42);
        assert_eq!(r.input.steps, 20);
        assert_eq!(r.input.effects, self.effects);
        if !self.effects {
            assert_eq!(r.input.mask, vec![0, 255, 255, 0], "protected cells must not become inpainting holes");
        }
        if matches!(self.outcome, Outcome::Fail) {
            return Err("synthetic failure".into());
        }
        let mut output = RetouchOutput {
            width: 4,
            height: 1,
            rgb: vec![if self.effects { [9., 8., 7.] } else { [0.75, 0.5, 0.25] }; 4],
            coverage: self.effects.then_some(vec![255, 255, 255, 0]),
            provenance: RetouchProvenance {
                model: if self.effects { "ObjectClear" } else { "RORem" }.into(),
                revision: "1".into(),
                identity: r.identity.clone(),
            },
        };
        match self.outcome {
            Outcome::BadCoverage => output.coverage = Some(vec![255, 0, 255, 0]),
            Outcome::WrongIdentity => output.provenance.identity.runtime = "wrong".into(),
            Outcome::WrongRevision => output.provenance.revision = "wrong".into(),
            Outcome::Nonfinite => output.rgb[0][0] = f32::NAN,
            Outcome::Cancel => cancel.store(true, Ordering::Relaxed),
            _ => {}
        }
        Ok(output)
    }
}
fn prepared() -> PreparedInput {
    PreparedInput {
        request: RetouchInput {
            width: 4,
            height: 1,
            rgb: vec![[0.125, 0.25, 0.5]; 4],
            mask: vec![0, 255, 0, 0],
            rect: [0., 0., 1., 1.],
            seed: 42,
            steps: 20,
            effects: true,
            protected: vec![true, false, false, false],
        },
        support: None,
    }
}
fn stages() -> [AiEditStage; 2] {
    [true, false].map(|effects| AiEditStage {
        role: if effects { AiStageRole::Effects } else { AiStageRole::Reconstruction },
        backend: if effects { "objectclear" } else { "rorem" }.into(),
        model_id: if effects { "first-model" } else { "second-model" }.into(),
        model_revision: "1".into(),
        runtime_id: if effects { "first-runtime" } else { "second-runtime" }.into(),
        binary_sha256: "a".repeat(64),
        model_manifest_sha256: "b".repeat(64),
        mask_sha256: String::new(),
        seed: 42,
        steps: 20,
        effects: effects.then_some(AiEffectsContract::ObjectEffectsV1),
    })
}
fn run(first: &Fake, second: &Fake, cancel: &AtomicBool) -> Result<AiEdit, String> {
    execute_chain_with_progress(prepared(), first, second, stages(), cancel, Instant::now() + Duration::from_secs(5), &|_| {})
}
#[test]
fn chain_uses_original_rgb_expanded_unprotected_mask_and_persists_one_final_generator() {
    let first = Fake::new(true, Outcome::Valid);
    let second = Fake::new(false, Outcome::Valid);
    let edit = run(&first, &second, &AtomicBool::new(false)).unwrap();
    assert_eq!((first.calls.get(), second.calls.get()), (1, 1));
    assert_eq!(edit.model, "RORem");
    assert_eq!(edit.model_version, "1");
    assert_eq!(edit.seed, 42);
    assert_eq!(edit.decode_pixels().unwrap(), vec![[0.125, 0.25, 0.5, 0.], [0.75, 0.5, 0.25, 1.], [0.75, 0.5, 0.25, 1.], [0.125, 0.25, 0.5, 0.]]);
    assert_eq!(edit.stages.len(), 2);
    assert_ne!(edit.stages[0].mask_sha256, edit.stages[1].mask_sha256);
    let restored: AiEdit = serde_json::from_value(serde_json::to_value(&edit).unwrap()).unwrap();
    assert_eq!(restored, edit);
}
#[test]
fn first_stage_failures_never_launch_reconstruction() {
    for outcome in [Outcome::Fail, Outcome::BadCoverage, Outcome::WrongIdentity, Outcome::WrongRevision, Outcome::Nonfinite, Outcome::Cancel] {
        let first = Fake::new(true, outcome);
        let second = Fake::new(false, Outcome::Valid);
        assert!(run(&first, &second, &AtomicBool::new(false)).is_err());
        assert_eq!(second.calls.get(), 0);
    }
}
#[test]
fn second_stage_failure_provenance_or_cancel_never_produces_a_patch() {
    for outcome in [Outcome::Fail, Outcome::WrongIdentity, Outcome::WrongRevision, Outcome::Nonfinite, Outcome::Cancel, Outcome::BadCoverage] {
        let first = Fake::new(true, Outcome::Valid);
        let second = Fake::new(false, outcome);
        assert!(run(&first, &second, &AtomicBool::new(false)).is_err());
        assert_eq!(second.calls.get(), 1);
    }
}
#[test]
fn cancelled_between_stages_or_before_finalization_never_produces_a_draft() {
    for phase in [super::super::Phase::Reconstruction, super::super::Phase::Finalizing] {
        let cancel = AtomicBool::new(false);
        let first = Fake::new(true, Outcome::Valid);
        let second = Fake::new(false, Outcome::Valid);
        assert!(
            execute_chain_with_progress(
                prepared(),
                &first,
                &second,
                stages(),
                &cancel,
                Instant::now() + Duration::from_secs(5),
                &|p| if p == phase {
                    cancel.store(true, Ordering::Relaxed)
                }
            )
            .is_err()
        );
        if phase == super::super::Phase::Reconstruction {
            assert_eq!(second.calls.get(), 0);
        }
    }
}
#[test]
fn expired_budget_and_wholly_protected_coverage_do_not_launch_second_stage() {
    let first = Fake::new(true, Outcome::Valid);
    let second = Fake::new(false, Outcome::Valid);
    assert!(execute_chain_with_progress(prepared(), &first, &second, stages(), &AtomicBool::new(false), Instant::now(), &|_| {}).is_err());
    assert_eq!(first.calls.get(), 0);
    let mut input = prepared();
    input.request.protected.fill(true);
    assert!(
        execute_chain_with_progress(input, &first, &second, stages(), &AtomicBool::new(false), Instant::now() + Duration::from_secs(5), &|_| {})
            .unwrap_err()
            .contains("No unprotected")
    );
    assert_eq!(second.calls.get(), 0);
}
