use std::sync::atomic::{AtomicUsize, Ordering};
use std::{env, process};

use crate::update::{DiscoverySupport, VendorUpdateResult, *};

struct FakeMaintenance {
    provider: ProviderKind,
    probes: AtomicUsize,
    updates: AtomicUsize,
}

struct LockedMaintenance;

impl ProviderMaintenance for LockedMaintenance {
    fn provider(&self) -> ProviderKind {
        ProviderKind::Codex
    }

    fn probe(&self, _: &AgentCli) -> Result<VersionStatus, UpdateError> {
        Ok(VersionStatus {
            provider: ProviderKind::Codex,
            current: Some(Version::new(1, 0, 0)),
            available: Some(Version::new(1, 1, 0)),
            install_method: Some("fake".into()),
            channel: None,
            can_update: true,
            support: DiscoverySupport::Supported,
            remediation: None,
        })
    }

    fn update(&self, _: &AgentCli) -> Result<VendorUpdateResult, UpdateError> {
        Err(UpdateError::new(
            UpdateErrorKind::ExternalLock,
            "provider files are locked",
        ))
    }
}

impl ProviderMaintenance for FakeMaintenance {
    fn provider(&self) -> ProviderKind {
        self.provider
    }

    fn probe(&self, _: &AgentCli) -> Result<VersionStatus, UpdateError> {
        self.probes.fetch_add(1, Ordering::SeqCst);
        Ok(VersionStatus {
            provider: self.provider,
            current: Some(Version::new(1, 0, 0)),
            available: Some(Version::new(1, 1, 0)),
            install_method: Some("fake".into()),
            channel: Some("latest".into()),
            can_update: true,
            support: DiscoverySupport::Supported,
            remediation: Some("do-not-cache-provider-command".into()),
        })
    }

    fn update(&self, _: &AgentCli) -> Result<VendorUpdateResult, UpdateError> {
        self.updates.fetch_add(1, Ordering::SeqCst);
        Ok(VendorUpdateResult {
            diagnostic: "updated".into(),
        })
    }
}

fn test_path(name: &str) -> PathBuf {
    env::temp_dir().join(format!("niumaterm-update-{name}-{}.json", process::id()))
}

#[test]
fn fresh_cache_is_reused_and_manual_check_bypasses_it() {
    let path = test_path("cache");
    let now = DateTime::parse_from_rfc3339("2026-08-07T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let coordinator = UpdateCoordinator::with_clock(path.clone(), Arc::new(move || now));
    let fake = Arc::new(FakeMaintenance {
        provider: ProviderKind::Codex,
        probes: AtomicUsize::new(0),
        updates: AtomicUsize::new(0),
    });
    let key = coordinator.register(
        ProviderKind::Codex,
        AgentCli::new("fake-codex", []),
        fake.clone(),
    );

    coordinator.check(&key, false).unwrap();
    coordinator.check(&key, false).unwrap();
    assert_eq!(fake.probes.load(Ordering::SeqCst), 1);
    assert!(
        !fs::read_to_string(&path)
            .unwrap()
            .contains("do-not-cache-provider-command")
    );
    coordinator.check(&key, true).unwrap();
    assert_eq!(fake.probes.load(Ordering::SeqCst), 2);
    let _ = fs::remove_file(path);
}

#[test]
fn operation_claim_serializes_updates_and_dismissal_is_version_keyed() {
    let path = test_path("claim");
    let coordinator = UpdateCoordinator::new(path.clone());
    let fake = Arc::new(FakeMaintenance {
        provider: ProviderKind::Claude,
        probes: AtomicUsize::new(0),
        updates: AtomicUsize::new(0),
    });
    let key = coordinator.register(
        ProviderKind::Claude,
        AgentCli::new("fake-claude", []),
        fake.clone(),
    );
    let duplicate_key = coordinator.register(
        ProviderKind::Claude,
        AgentCli::new("fake-claude", []),
        fake.clone(),
    );
    assert_eq!(key, duplicate_key);
    assert_eq!(coordinator.snapshots().len(), 1);
    coordinator.check(&key, true).unwrap();
    coordinator.begin_update(&key).unwrap();
    assert!(coordinator.begin_update(&key).is_err());
    coordinator.run_vendor_update(&key).unwrap();
    assert_eq!(fake.updates.load(Ordering::SeqCst), 1);

    let target = Version::new(1, 1, 0);
    coordinator.dismiss_available(&key, &target);
    assert_eq!(
        coordinator.snapshot(&key).unwrap().dismissed_target,
        Some(target)
    );
    coordinator.finish_update(
        &key,
        None,
        Some(UpdateError::new(UpdateErrorKind::ProviderFailed, "failed")),
        0,
    );
    let _ = fs::remove_file(path);
}

#[test]
fn unchanged_and_partial_recovery_outcomes_keep_verified_versions() {
    let path = test_path("outcomes");
    let coordinator = UpdateCoordinator::new(path.clone());
    let fake = Arc::new(FakeMaintenance {
        provider: ProviderKind::Claude,
        probes: AtomicUsize::new(0),
        updates: AtomicUsize::new(0),
    });
    let key = coordinator.register(
        ProviderKind::Claude,
        AgentCli::new("fake-outcomes-claude", []),
        fake,
    );
    let available = coordinator.check(&key, true).unwrap();
    coordinator.begin_update(&key).unwrap();
    coordinator.finish_update(&key, Some(available), None, 0);
    let unchanged = coordinator.snapshot(&key).unwrap();
    assert_eq!(unchanged.state.phase, UpdatePhase::Unchanged);
    assert_eq!(
        unchanged.state.error.unwrap().kind,
        UpdateErrorKind::ProviderFailed
    );

    coordinator.begin_update(&key).unwrap();
    let verified = VersionStatus {
        provider: ProviderKind::Claude,
        current: Some(Version::new(1, 1, 0)),
        available: Some(Version::new(1, 1, 0)),
        install_method: Some("fake".into()),
        channel: Some("latest".into()),
        can_update: true,
        support: DiscoverySupport::Supported,
        remediation: None,
    };
    coordinator.finish_update(
        &key,
        Some(verified),
        Some(UpdateError::new(
            UpdateErrorKind::Recovery,
            "one tab could not reconnect",
        )),
        0,
    );
    let partial = coordinator.snapshot(&key).unwrap();
    assert_eq!(partial.state.phase, UpdatePhase::Failed);
    assert_eq!(
        partial.state.versions.unwrap().current,
        Some(Version::new(1, 1, 0))
    );
    let _ = fs::remove_file(path);
}

#[test]
fn updater_external_lock_is_preserved_as_an_actionable_failure() {
    let path = test_path("external-lock");
    let coordinator = UpdateCoordinator::new(path.clone());
    let key = coordinator.register(
        ProviderKind::Codex,
        AgentCli::new("fake-locked-codex", []),
        Arc::new(LockedMaintenance),
    );
    coordinator.check(&key, true).unwrap();
    coordinator.begin_update(&key).unwrap();
    let error = coordinator.run_vendor_update(&key).unwrap_err();
    assert_eq!(error.kind, UpdateErrorKind::ExternalLock);
    coordinator.finish_update(&key, None, Some(error), 0);

    let failed = coordinator.snapshot(&key).unwrap();
    assert_eq!(failed.state.phase, UpdatePhase::Failed);
    assert_eq!(
        failed.state.error.unwrap().kind,
        UpdateErrorKind::ExternalLock
    );
    let _ = fs::remove_file(path);
}
