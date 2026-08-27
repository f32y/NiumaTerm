use std::time::Duration;
use std::{env, process};

use chrono::Utc;
use nmt_agent_utils::launcher::AgentCli;
use nmt_agent_utils::update::{
    DiscoverySupport, InstallationKey, InstallationSnapshot, InstallationUpdateState, ProviderKind,
    ProviderMaintenance, UpdateError, UpdateErrorKind, UpdatePhase, UpdateProgress, VersionStatus,
};
use nmt_app_agent::{AgentKind, RecoveryIdentity, RecoveryReadiness, RecoverySnapshot};
use semver::Version;

use crate::agent_updates::*;

fn snapshot(phase: UpdatePhase) -> InstallationSnapshot {
    let launcher = AgentCli::new("fake-codex", []);
    let identity = InstallationKey::derive(ProviderKind::Codex, &launcher);
    InstallationSnapshot {
        identity,
        state: InstallationUpdateState {
            phase,
            versions: Some(VersionStatus {
                provider: ProviderKind::Codex,
                current: Some(Version::new(1, 0, 0)),
                available: Some(Version::new(1, 1, 0)),
                install_method: Some("fake".into()),
                channel: None,
                can_update: true,
                support: DiscoverySupport::Supported,
                remediation: None,
            }),
            progress: None,
            error: None,
        },
        last_checked: Some(Utc::now()),
        dismissed_target: None,
        busy: false,
        notification_hidden: false,
    }
}

#[test]
fn reducer_keeps_identity_and_maps_phase_actions() {
    let available = notification_view(&snapshot(UpdatePhase::Available)).unwrap();
    let mut running = snapshot(UpdatePhase::Suspending);
    running.state.progress = Some(UpdateProgress {
        completed: 1,
        total: 2,
    });
    let running = notification_view(&running).unwrap();
    let failed = notification_view(&InstallationSnapshot {
        state: InstallationUpdateState {
            phase: UpdatePhase::Failed,
            error: Some(UpdateError::new(UpdateErrorKind::ExternalLock, "locked")),
            ..snapshot(UpdatePhase::Failed).state
        },
        ..snapshot(UpdatePhase::Failed)
    })
    .unwrap();

    assert_eq!(available.key, running.key);
    assert_eq!(available.primary, Some(NotificationPrimaryAction::Update));
    assert_eq!(running.progress, NotificationProgress::Determinate(50.0));
    assert_eq!(failed.primary, Some(NotificationPrimaryAction::Retry));
    assert!(!failed.terminal_timeout);
}

#[test]
fn dismissal_is_scoped_to_the_reported_target() {
    let mut dismissed = snapshot(UpdatePhase::Available);
    dismissed.dismissed_target = Some(Version::new(1, 1, 0));
    assert!(notification_view(&dismissed).is_none());
    dismissed.state.versions.as_mut().unwrap().available = Some(Version::new(1, 2, 0));
    assert!(notification_view(&dismissed).is_some());

    let mut running = snapshot(UpdatePhase::Updating);
    running.dismissed_target = Some(Version::new(1, 1, 0));
    assert!(notification_view(&running).is_some());
    running.notification_hidden = true;
    assert!(notification_view(&running).is_none());
}

#[test]
fn terminal_lifetime_counts_only_focused_visible_time_and_resets_by_phase() {
    let mut lifetime = FocusedVisibleLifetime::new(UpdatePhase::Updated);
    assert!(!lifetime.tick(false, Duration::from_secs(5)));
    assert!(!lifetime.tick(true, Duration::from_millis(2_999)));
    assert!(lifetime.tick(true, Duration::from_millis(1)));

    lifetime.set_phase(UpdatePhase::Unchanged);
    assert!(!lifetime.tick(true, Duration::from_millis(2_999)));
    assert!(lifetime.tick(true, Duration::from_millis(1)));
}

#[test]
fn reducer_stacks_installations_and_never_fabricates_provider_progress() {
    let first = snapshot(UpdatePhase::Updating);
    let mut second = snapshot(UpdatePhase::Available);
    second.identity =
        InstallationKey::derive(ProviderKind::Codex, &AgentCli::new("another-codex", []));
    second.state.versions.as_mut().unwrap().can_update = false;

    let first_view = notification_view(&first).unwrap();
    let second_view = notification_view(&second).unwrap();
    assert_ne!(first_view.key, second_view.key);
    assert_eq!(first_view.progress, NotificationProgress::Indeterminate);
    assert_eq!(first_view.primary, None);
    assert_eq!(second_view.primary, None);
    assert!(second_view.show_settings);
}

#[test]
fn failed_diagnostics_are_bounded_and_persistent() {
    let mut failed = snapshot(UpdatePhase::Failed);
    failed.state.error = Some(UpdateError::new(
        UpdateErrorKind::ProviderFailed,
        "x".repeat(10_000),
    ));
    let view = notification_view(&failed).unwrap();
    assert!(view.message.chars().count() <= 512);
    assert!(!view.terminal_timeout);
    assert_eq!(view.primary, Some(NotificationPrimaryAction::Retry));
    assert!(view.show_settings);
}

#[test]
fn testing_mode_uses_only_fake_maintenance_and_a_process_local_cache() {
    let testing_cache = update_cache_path(true);
    assert!(testing_cache.starts_with(env::temp_dir()));
    assert!(
        testing_cache
            .to_string_lossy()
            .contains(&process::id().to_string())
    );
    assert_ne!(testing_cache, update_cache_path(false));

    let fake = FakeMaintenance::new(ProviderKind::Claude);
    let launcher = AgentCli::new("this-executable-must-never-run", []);
    assert!(fake.probe(&launcher).unwrap().update_available());
    fake.update(&launcher).unwrap();
    assert!(!fake.probe(&launcher).unwrap().update_available());
}

#[test]
fn mixed_installations_select_only_tabs_for_the_target_transaction() {
    let shared =
        InstallationKey::derive(ProviderKind::Claude, &AgentCli::new("shared-claude", [])).key;
    let unrelated =
        InstallationKey::derive(ProviderKind::Claude, &AgentCli::new("other-claude", [])).key;
    // The `None` stands for a tab whose harness updates outside the
    // application; it must not be suspended by anyone else's transaction.
    let installations = vec![
        Some(shared.clone()),
        Some(unrelated),
        Some(shared.clone()),
        None,
    ];
    assert_eq!(
        affected_installation_indices(&shared, &installations),
        vec![0, 2]
    );
}

#[test]
fn settings_installation_rows_deduplicate_profiles_in_first_seen_order() {
    let claude =
        InstallationKey::derive(ProviderKind::Claude, &AgentCli::new("shared-claude", [])).key;
    let codex =
        InstallationKey::derive(ProviderKind::Codex, &AgentCli::new("shared-codex", [])).key;

    assert_eq!(
        distinct_installation_keys([claude.clone(), claude.clone(), codex.clone(), claude,]),
        vec![
            InstallationKey::derive(ProviderKind::Claude, &AgentCli::new("shared-claude", []),).key,
            codex,
        ]
    );
}

#[test]
fn preflight_waits_or_interrupts_without_skipping_recovery_validation() {
    assert!(!UpdateMode::WhenIdle.interrupts_active_work());
    assert!(UpdateMode::StopNow.interrupts_active_work());

    assert!(matches!(
        resolve_preflight(
            vec![RecoveryReadiness::Busy("turn running".into())],
            UpdateMode::WhenIdle,
            true,
        ),
        PreflightResolution::Wait
    ));
    assert!(matches!(
        resolve_preflight(
            vec![RecoveryReadiness::Busy("interrupt pending".into())],
            UpdateMode::StopNow,
            false,
        ),
        PreflightResolution::Wait
    ));
    assert!(matches!(
        resolve_preflight(
            vec![RecoveryReadiness::Busy("interrupt stuck".into())],
            UpdateMode::StopNow,
            true,
        ),
        PreflightResolution::Failed(message)
            if message.contains("recoverable interruption boundary")
    ));
    assert!(matches!(
        resolve_preflight(
            vec![RecoveryReadiness::MissingIdentity("missing session id".into())],
            UpdateMode::StopNow,
            false,
        ),
        PreflightResolution::Failed(message) if message == "missing session id"
    ));
}

#[test]
fn preflight_retains_every_ready_tab_and_aggregates_partial_resume_failure() {
    let assessments = vec![
        RecoveryReadiness::Ready(RecoverySnapshot {
            identity: Some(RecoveryIdentity::new(AgentKind::Claude, "session-a")),
            profile_name: "Claude A".into(),
        }),
        RecoveryReadiness::Ready(RecoverySnapshot {
            identity: Some(RecoveryIdentity::new(AgentKind::Claude, "session-b")),
            profile_name: "Claude B".into(),
        }),
    ];
    let PreflightResolution::Ready(snapshots) =
        resolve_preflight(assessments, UpdateMode::WhenIdle, false)
    else {
        panic!("ready tabs should pass preflight");
    };
    assert_eq!(snapshots.len(), 2);

    let combined = combine_transaction_error(
        Some(UpdateError::new(
            UpdateErrorKind::ProviderFailed,
            "updater failed",
        )),
        1,
    )
    .unwrap();
    assert_eq!(combined.kind, UpdateErrorKind::Recovery);
    assert!(combined.message().contains("updater failed"));
    assert!(combined.message().contains("1 agent tab"));
}
