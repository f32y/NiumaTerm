use std::time::Duration;

use futures::executor::block_on;
use nmt_agent::session::lifecycle::{RecoveryReadiness, RecoverySnapshot, RestorationReadiness};
use nmt_agent::session::{AgentKind, RecoveryIdentity};
use nmt_agent::update::{
    DiscoverySupport, ProviderKind, UpdateError, UpdateErrorKind, UpdatePhase, UpdateProgress,
    VersionStatus,
};

use crate::agent_updates::maintenance::{
    PreflightFailure, UpdateEnvironment, UpdateMode, run_transaction,
};

struct MemoryEnvironment {
    now: Duration,
    idle_after: Duration,
    missing_identity: Option<String>,
    snapshots: Vec<RecoverySnapshot>,
    suspensions: Vec<Result<(), String>>,
    update_error: Option<UpdateError>,
    verify_error: Option<UpdateError>,
    restoration: Vec<RestorationReadiness>,
    restored: Vec<(usize, RecoverySnapshot)>,
    timed_out: Vec<usize>,
    operations: Vec<&'static str>,
    progress: Vec<(UpdatePhase, Option<UpdateProgress>)>,
}

impl MemoryEnvironment {
    fn new(count: usize) -> Self {
        Self {
            now: Duration::ZERO,
            idle_after: Duration::ZERO,
            missing_identity: None,
            snapshots: (0..count)
                .map(|index| RecoverySnapshot {
                    identity: Some(RecoveryIdentity::new(
                        AgentKind::Claude,
                        format!("session-{index}"),
                    )),
                    profile_name: format!("profile-{index}"),
                })
                .collect(),
            suspensions: vec![Ok(()); count],
            update_error: None,
            verify_error: None,
            restoration: vec![RestorationReadiness::Ready; count],
            restored: Vec::new(),
            timed_out: Vec::new(),
            operations: Vec::new(),
            progress: Vec::new(),
        }
    }
}

impl UpdateEnvironment for MemoryEnvironment {
    fn identity_failure(&mut self) -> Option<String> {
        self.missing_identity.clone()
    }

    fn prepare(&mut self, mode: UpdateMode) {
        self.operations.push(match mode {
            UpdateMode::WhenIdle => "wait",
            UpdateMode::StopNow => "interrupt",
        });
    }

    fn readiness(&mut self) -> Vec<RecoveryReadiness> {
        if let Some(message) = &self.missing_identity {
            return vec![RecoveryReadiness::MissingIdentity(message.clone())];
        }

        self.snapshots
            .iter()
            .map(|snapshot| {
                if self.now < self.idle_after {
                    RecoveryReadiness::Busy("turn running".into())
                } else {
                    RecoveryReadiness::Ready(snapshot.clone())
                }
            })
            .collect()
    }

    fn cancel_wait(&mut self) {
        self.operations.push("cancel");
    }

    async fn suspend(&mut self, _: UpdateMode) -> Vec<Result<(), String>> {
        self.operations.push("suspend");

        self.suspensions.clone()
    }

    async fn update(&mut self) -> Result<(), UpdateError> {
        self.operations.push("update");

        self.update_error.clone().map_or(Ok(()), Err)
    }

    async fn verify(&mut self) -> Result<VersionStatus, UpdateError> {
        self.operations.push("verify");

        if let Some(error) = &self.verify_error {
            return Err(error.clone());
        }

        Ok(VersionStatus {
            provider: ProviderKind::Claude,
            current: Some("2.0.0".parse().unwrap()),
            available: Some("2.0.0".parse().unwrap()),
            install_method: None,
            channel: None,
            can_update: true,
            support: DiscoverySupport::Supported,
            remediation: None,
        })
    }

    fn restore(&mut self, snapshots: &[RecoverySnapshot], suspended: &[usize]) {
        self.operations.push("restore");

        self.restored.extend(
            suspended
                .iter()
                .map(|&index| (index, snapshots[index].clone())),
        );
    }

    fn restoration_readiness(&mut self, suspended: &[usize]) -> Vec<RestorationReadiness> {
        suspended
            .iter()
            .map(|&index| self.restoration[index].clone())
            .collect()
    }

    fn recovery_timed_out(&mut self, pending: &[usize]) {
        self.timed_out.extend_from_slice(pending);
    }

    fn publish(&mut self, phase: UpdatePhase, progress: Option<UpdateProgress>) {
        self.progress.push((phase, progress));
    }

    fn now(&self) -> Duration {
        self.now
    }

    async fn wait(&mut self, duration: Duration) {
        self.now += duration;
    }
}

#[test]
fn idle_update_waits_beyond_the_interrupt_deadline_and_restores_each_identity() {
    let mut environment = MemoryEnvironment::new(2);

    environment.idle_after = Duration::from_secs(20);

    let outcome = block_on(run_transaction(&mut environment, UpdateMode::WhenIdle)).unwrap();

    assert_eq!(environment.now, Duration::from_secs(20));
    assert_eq!(
        environment.operations,
        ["wait", "suspend", "update", "verify", "restore"]
    );
    assert_eq!(
        environment.restored,
        environment
            .snapshots
            .iter()
            .cloned()
            .enumerate()
            .collect::<Vec<_>>()
    );
    assert!(outcome.verified.is_some());
    assert!(outcome.operation_error.is_none());
    assert_eq!(outcome.restore_failures, 0);
    assert_eq!(
        environment.progress.last(),
        Some(&(
            UpdatePhase::Restoring,
            Some(UpdateProgress {
                completed: 2,
                total: 2
            })
        ))
    );
}

#[test]
fn force_update_checks_identity_before_interrupting_and_bounds_the_wait() {
    let mut missing = MemoryEnvironment::new(1);

    missing.missing_identity = Some("missing session id".into());

    let error = block_on(run_transaction(&mut missing, UpdateMode::StopNow))
        .err()
        .unwrap();

    assert_eq!(
        error,
        PreflightFailure::MissingIdentity("missing session id".into())
    );
    assert!(missing.operations.is_empty());

    let mut busy = MemoryEnvironment::new(1);

    busy.idle_after = Duration::from_secs(16);

    let error = block_on(run_transaction(&mut busy, UpdateMode::StopNow))
        .err()
        .unwrap();

    assert_eq!(error, PreflightFailure::InterruptionTimeout);
    assert_eq!(busy.operations, ["interrupt", "cancel"]);
    assert_eq!(busy.now, Duration::from_secs(15));

    let error = block_on(run_transaction(&mut missing, UpdateMode::WhenIdle))
        .err()
        .unwrap();

    assert_eq!(
        error,
        PreflightFailure::MissingIdentity("missing session id".into())
    );
    assert_eq!(missing.operations, ["wait", "cancel"]);
}

#[test]
fn partial_suspension_failure_restores_only_stopped_sessions_and_reports_recovery_failures() {
    let mut environment = MemoryEnvironment::new(3);

    environment.suspensions[1] = Err("shutdown failed".into());
    environment.restoration[0] = RestorationReadiness::Failed("restart failed".into());
    environment.restoration[2] = RestorationReadiness::Pending;

    let outcome = block_on(run_transaction(&mut environment, UpdateMode::StopNow)).unwrap();

    assert_eq!(environment.operations, ["interrupt", "suspend", "restore"]);
    assert_eq!(
        environment.restored,
        vec![
            (0, environment.snapshots[0].clone()),
            (2, environment.snapshots[2].clone())
        ]
    );
    assert_eq!(environment.timed_out, [2]);
    assert_eq!(environment.now, Duration::from_secs(30));
    assert_eq!(
        outcome.operation_error.unwrap().message(),
        "shutdown failed"
    );
    assert_eq!(outcome.restore_failures, 2);
    assert!(outcome.verified.is_none());
}

#[test]
fn update_and_verification_failures_both_restore_all_suspended_sessions() {
    for failing_stage in ["update", "verify"] {
        let mut environment = MemoryEnvironment::new(2);
        let failure = UpdateError::new(UpdateErrorKind::ProviderFailed, failing_stage);

        if failing_stage == "update" {
            environment.update_error = Some(failure);
        } else {
            environment.verify_error = Some(failure);
        }

        let outcome = block_on(run_transaction(&mut environment, UpdateMode::WhenIdle)).unwrap();

        assert_eq!(outcome.operation_error.unwrap().message(), failing_stage);
        assert_eq!(outcome.restore_failures, 0);
        assert_eq!(environment.restored.len(), 2);
        assert_eq!(environment.operations.last(), Some(&"restore"));
        assert_eq!(
            environment.operations.contains(&"verify"),
            failing_stage == "verify"
        );
    }
}

#[test]
fn an_installation_without_open_sessions_still_updates_and_verifies() {
    let mut environment = MemoryEnvironment::new(0);
    let outcome = block_on(run_transaction(&mut environment, UpdateMode::WhenIdle)).unwrap();

    assert!(outcome.verified.is_some());
    assert_eq!(outcome.restore_failures, 0);
    assert!(environment.restored.is_empty());
    assert_eq!(environment.now, Duration::ZERO);
}
