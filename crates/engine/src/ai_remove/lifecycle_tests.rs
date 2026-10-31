//! Deterministic channel-controlled jobs exercise UI state without models or subprocesses.
use super::*;
use std::sync::{
    Arc,
    atomic::AtomicBool,
    mpsc::{Sender, channel},
};

type Reply = std::result::Result<AiEdit, String>;
fn pending(s: &mut Session) -> (Sender<Reply>, Sender<Phase>, crate::activity::TaskGuard, AiEdit) {
    tests::draft(s);
    let edit = s.ai_remove.draft.take().unwrap();
    let (tx, reply) = channel();
    let (stages, phases) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let task = s.activity.start("retouch", "AI Remove", crate::activity::Cancel::Flag(cancel.clone()));
    s.ai_remove.job = Some(Job { reply, phases, cancel });
    s.ai_remove.phase = Phase::Loading;
    (tx, stages, task, edit)
}

#[test]
fn cancel_keeps_receiver_and_activity_until_ack_and_discards_late_success() {
    let mut s = Session::with_demo();
    let (tx, _, task, edit) = pending(&mut s);
    s.execute("aiRemove.cancel", &json!({})).unwrap();
    s.execute("aiRemove.cancel", &json!({})).unwrap();
    let status = s.execute("aiRemove.status", &json!({})).unwrap();
    assert_eq!(status["phase"], "cancelling");
    assert_eq!(status["running"], true);
    assert_eq!(status["ready"], false);
    assert!(s.activity.list()[0].cancelling);
    assert!(s.execute("aiRemove.start", &json!({})).unwrap_err().to_string().contains("already running"));
    drop(task); // Real worker drops its activity guard before sending acknowledgement.
    tx.send(Ok(edit)).unwrap(); // The cancelled job retained its reply receiver.
    s.ai_remove_poll();
    assert!(!s.ai_remove.busy());
    assert!(s.ai_remove.draft.is_none());
    assert_eq!(s.ai_remove.phase, Phase::Cancelled);
    assert!(s.activity.list().is_empty());
}

#[test]
fn activity_cancel_uses_same_flag_and_blocks_late_success() {
    let mut s = Session::with_demo();
    let (tx, _, task, edit) = pending(&mut s);
    s.execute("activity.cancel", &json!({"id":task.id()})).unwrap();
    s.ai_remove_poll();
    assert_eq!(s.ai_remove.phase, Phase::Cancelling);
    assert!(s.ai_remove.busy());
    assert!(s.activity.list()[0].cancelling);
    drop(task);
    tx.send(Ok(edit)).unwrap();
    s.ai_remove_poll();
    assert_eq!(s.ai_remove.phase, Phase::Cancelled);
    assert!(s.ai_remove.draft.is_none());
}

#[test]
fn phase_updates_ready_and_failure_are_additive_status_fields() {
    let mut s = Session::with_demo();
    assert_eq!(s.execute("aiRemove.status", &json!({})).unwrap()["phase"], "idle");
    let (tx, stages, task, edit) = pending(&mut s);
    for phase in [Phase::Preparing, Phase::Running, Phase::Finalizing] {
        stages.send(phase).unwrap();
        s.ai_remove_poll();
        assert_eq!(s.ai_remove.phase, phase);
        assert!(s.ai_remove.busy());
    }
    drop(task);
    tx.send(Ok(edit)).unwrap();
    let status = s.execute("aiRemove.status", &json!({})).unwrap();
    assert_eq!(status["phase"], "ready");
    assert_eq!(status["running"], false);
    assert_eq!(status["ready"], true);
    let (tx, _, task, _) = pending(&mut s);
    drop(task);
    tx.send(Err("synthetic backend error".into())).unwrap();
    s.ai_remove_poll();
    assert_eq!(s.ai_remove.phase, Phase::Failed);
    assert!(s.ai_remove.message.contains("synthetic backend error"));
    assert!(s.ai_remove.draft.is_none());
}

#[test]
fn edit_and_photo_invalidation_keep_worker_until_ack() {
    for change in ["edit", "photo"] {
        let mut s = Session::with_demo();
        let (tx, _, task, edit) = pending(&mut s);
        if change == "edit" {
            s.execute("develop.set", &json!({"control":"light.exposure","value":0.5})).unwrap();
        } else {
            s.execute("library.down", &json!({})).unwrap();
        }
        s.ai_remove_poll();
        assert_eq!(s.ai_remove.phase, Phase::Cancelling);
        assert!(s.ai_remove.busy());
        assert!(s.ai_remove.message.contains("discarded"));
        drop(task);
        tx.send(Ok(edit)).unwrap();
        s.ai_remove_poll();
        assert_eq!(s.ai_remove.phase, Phase::Cancelled);
        assert!(s.ai_remove.draft.is_none());
    }
}

#[test]
fn disconnected_worker_fails_without_stuck_busy_or_activity() {
    let mut s = Session::with_demo();
    let (tx, _, task, _) = pending(&mut s);
    drop(task);
    drop(tx);
    s.ai_remove_poll();
    assert!(!s.ai_remove.busy());
    assert_eq!(s.ai_remove.phase, Phase::Failed);
    assert!(s.ai_remove.message.contains("stopped unexpectedly"));
    assert!(s.activity.list().is_empty());
}

#[test]
fn worker_panic_is_reported_after_activity_cleanup() {
    let activity = crate::activity::Activity::default();
    let job = Job::spawn(&activity, "synthetic", |_, _| panic!("synthetic worker failure")).unwrap();
    let result = job.reply.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(result.unwrap_err().contains("failed unexpectedly: synthetic worker failure"));
    assert!(activity.list().is_empty());
}

#[test]
fn spawned_worker_keeps_activity_until_blocked_work_acknowledges_cancel() {
    let activity = crate::activity::Activity::default();
    let (entered_tx, entered_rx) = channel();
    let (release_tx, release_rx) = channel();
    let job = Job::spawn(&activity, "synthetic", move |cancel, progress| {
        progress(Phase::Running);
        entered_tx.send(()).map_err(|e| e.to_string())?;
        release_rx.recv_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
        assert!(cancel.load(std::sync::atomic::Ordering::Relaxed));
        Err("Cancelled.".into())
    })
    .unwrap();
    entered_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    let rows = activity.list();
    assert_eq!(rows.len(), 1);
    activity.cancel(rows[0].id).unwrap();
    assert!(activity.list()[0].cancelling);
    assert!(matches!(job.reply.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
    release_tx.send(()).unwrap();
    assert_eq!(job.reply.recv_timeout(std::time::Duration::from_secs(5)).unwrap().unwrap_err(), "Cancelled.");
    assert!(activity.list().is_empty());
}
