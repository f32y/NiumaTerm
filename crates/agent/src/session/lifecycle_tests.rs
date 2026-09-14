use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;

use crate::chat::{SendOutcome, SlashCommandOutcome};
use crate::session::backend::Backend;
use crate::session::lifecycle::{
    InterruptOutcome, RecoverySnapshot, RestorationReadiness, SessionRuntime, StartOutcome, Status,
    UpdateSuspension,
};
use crate::session::test_support::TestBackend;

fn backend() -> Backend {
    let mut backend = TestBackend::new([], SlashCommandOutcome::NotReady, Vec::new());

    backend.interrupt_accepted = true;

    Backend::Test(backend)
}

fn start(runtime: &mut SessionRuntime) -> u64 {
    let epoch = runtime.begin_start();

    assert!(matches!(
        runtime.install(epoch, Ok(backend())),
        StartOutcome::Installed
    ));

    runtime.ready();

    epoch
}

#[test]
fn replaced_session_rejects_output_exit_and_spawn_results() {
    let mut runtime = SessionRuntime::default();

    let old_epoch = start(&mut runtime);
    let current_epoch = start(&mut runtime);

    runtime.turn_started();

    assert!(runtime.process(old_epoch, json!({})).is_none());
    assert!(runtime.process_exit(old_epoch).is_none());
    assert!(matches!(
        runtime.install(old_epoch, Err("old failure".into())),
        StartOutcome::Superseded(None)
    ));

    let released = Arc::new(AtomicBool::new(false));

    let orphan = Backend::Test(
        TestBackend::new([], SlashCommandOutcome::NotReady, Vec::new())
            .watch_release(released.clone()),
    );

    let StartOutcome::Superseded(Some(orphan)) = runtime.install(old_epoch, Ok(orphan)) else {
        panic!("a replaced start must return its backend for shutdown");
    };

    assert!(!released.load(Ordering::SeqCst));
    assert_eq!(runtime.status(), Status::Running);
    assert!(runtime.start_failure().is_none());
    assert!(runtime.process(current_epoch, json!({})).is_some());

    drop(orphan);

    assert!(released.load(Ordering::SeqCst));
}

#[test]
fn start_failure_is_cleared_only_by_a_new_attempt() {
    let mut runtime = SessionRuntime::default();

    let epoch = runtime.begin_start();

    assert!(matches!(
        runtime.install(epoch, Err("launch failed".into())),
        StartOutcome::Failed(_)
    ));
    assert_eq!(runtime.status(), Status::Exited);
    assert_eq!(runtime.start_failure(), Some("launch failed"));

    runtime.begin_start();

    assert_eq!(runtime.status(), Status::Starting);
    assert!(runtime.start_failure().is_none());
    assert!(matches!(
        runtime.install(epoch, Err("late failure".into())),
        StartOutcome::Superseded(None)
    ));
    assert_eq!(runtime.status(), Status::Starting);
}

#[test]
fn ready_confirmation_does_not_end_an_active_turn() {
    let mut runtime = SessionRuntime::default();

    start(&mut runtime);

    runtime.turn_started();

    runtime.ready();

    assert_eq!(runtime.status(), Status::Running);
    assert!(!runtime.turn_completed(1));
    assert_eq!(runtime.status(), Status::Idle);

    runtime.turn_started();

    runtime.exited("connection lost");

    assert!(!runtime.turn_completed(2));
    assert_eq!(runtime.status(), Status::Exited);
}

#[test]
fn interruption_is_reported_when_the_matching_turn_completes() {
    let mut runtime = SessionRuntime::default();

    start(&mut runtime);

    runtime.turn_started();

    assert_eq!(runtime.interrupt(Some(7)), InterruptOutcome::Accepted);
    assert_eq!(runtime.status(), Status::Running);
    assert!(runtime.turn_completed(7));
    assert!(!runtime.turn_completed(7));

    runtime.turn_started();

    runtime.interrupt(Some(8));

    assert!(!runtime.turn_completed(9));
    assert!(!runtime.turn_completed(8));

    runtime.interrupt(Some(1));

    start(&mut runtime);

    assert!(!runtime.turn_completed(1));
}

#[test]
fn update_wait_blocks_sends_and_cancellation_restores_delivery() {
    let mut runtime = SessionRuntime::default();

    assert!(matches!(
        runtime.send(|_| panic!("no backend")),
        SendOutcome::NotReady
    ));

    start(&mut runtime);

    runtime.wait_for_update();

    assert!(matches!(
        runtime.send(|_| panic!("update wait must hold sends")),
        SendOutcome::NotReady
    ));
    assert!(runtime.cancel_update_wait());
    assert!(matches!(
        runtime.send(|_| SendOutcome::StartedTurn),
        SendOutcome::StartedTurn
    ));
    assert!(matches!(
        runtime.send(|_| SendOutcome::Steered),
        SendOutcome::Steered
    ));
    assert!(matches!(
        runtime.send(|_| SendOutcome::Rejected {
            message: "busy".into()
        }),
        SendOutcome::Rejected { .. }
    ));
}

#[test]
fn retained_backend_cannot_receive_sends_during_replacement_or_after_exit() {
    let mut runtime = SessionRuntime::default();

    start(&mut runtime);

    runtime.begin_start();

    assert!(runtime.backend().is_some());
    assert!(matches!(
        runtime.send(|_| panic!("retired conversation must not receive the draft")),
        SendOutcome::NotReady
    ));

    start(&mut runtime);

    runtime.exited("connection lost");

    assert!(matches!(
        runtime.send(|_| panic!("an exited session must not receive the draft")),
        SendOutcome::NotReady
    ));
}

#[test]
fn shutdown_rollback_cannot_replace_a_newer_session() {
    let mut runtime = SessionRuntime::default();

    let original_epoch = start(&mut runtime);
    let (shutdown_epoch, stopped) = runtime.suspend_for_update();

    assert!(runtime.backend().is_none());
    assert!(runtime.process_exit(original_epoch).is_none());
    assert_eq!(
        runtime.update_suspension(),
        Some(&UpdateSuspension::Stopping)
    );
    assert!(!runtime.cancel_update_wait());

    start(&mut runtime);

    runtime.turn_started();

    assert!(
        runtime
            .shutdown_failed(shutdown_epoch, stopped.expect("stopped backend"))
            .is_err()
    );
    assert_eq!(runtime.status(), Status::Running);
    assert!(runtime.backend().is_some());
}

#[test]
fn failed_shutdown_restores_the_current_backend() {
    let mut runtime = SessionRuntime::default();

    start(&mut runtime);

    let (epoch, backend) = runtime.suspend_for_update();

    assert!(
        runtime
            .shutdown_failed(epoch, backend.expect("stopped backend"))
            .is_ok()
    );
    assert_eq!(runtime.status(), Status::Idle);
    assert!(runtime.update_suspension().is_none());
    assert!(runtime.backend().is_some());
}

#[test]
fn recovery_stays_pending_until_ready_and_retains_retry_information() {
    let mut runtime = SessionRuntime::default();

    start(&mut runtime);

    runtime.suspend_for_update();

    runtime.provider_updating();

    let snapshot = RecoverySnapshot {
        identity: None,
        profile_name: "test".into(),
    };

    runtime.reconnect(Some(snapshot.clone()));

    let epoch = runtime.begin_start();

    assert!(matches!(
        runtime.install(epoch, Ok(backend())),
        StartOutcome::Installed
    ));
    assert_eq!(
        runtime.restoration_readiness(),
        RestorationReadiness::Pending
    );

    runtime.ready();

    assert_eq!(runtime.restoration_readiness(), RestorationReadiness::Ready);

    runtime.reconnect(None);

    runtime.exited("reconnect failed");

    assert_eq!(
        runtime.restoration_readiness(),
        RestorationReadiness::Failed("reconnect failed".into())
    );
    assert_eq!(runtime.last_recovery_snapshot(), Some(&snapshot));
}
