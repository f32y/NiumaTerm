use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Instant;
use std::{env, fs, process, thread};

use futures::FutureExt as _;
use futures::future::{BoxFuture, ready};
use nmt_platform::process::exit_status_from_code;

use crate::update::{DiscoverySupport, *};

#[test]
fn installation_keys_follow_platform_path_case_rules() {
    let upper = AgentCli::new("nmt-missing-installation/CLI", []);
    let lower = AgentCli::new("nmt-missing-installation/cli", []);
    let upper = InstallationKey::derive(ProviderKind::Codex, &upper);
    let lower = InstallationKey::derive(ProviderKind::Codex, &lower);

    #[cfg(windows)]
    assert_eq!(upper.key, lower.key);

    #[cfg(unix)]
    assert_ne!(upper.key, lower.key);
}

#[test]
fn installation_keys_dedupe_shared_launchers_and_split_update_contexts() {
    let first = AgentCli::new("codex", [("CODEX_HOME".to_string(), "C:\\A".to_string())]);
    let same = AgentCli::new("codex", [("CODEX_HOME".to_string(), "C:\\A".to_string())]);
    let other_home = AgentCli::new("codex", [("CODEX_HOME".to_string(), "C:\\B".to_string())]);

    let other_launcher = AgentCli::new(
        "definitely-distinct-codex.exe",
        [("CODEX_HOME".to_string(), "C:\\A".to_string())],
    );

    let identities = [
        InstallationKey::derive(ProviderKind::Codex, &first),
        InstallationKey::derive(ProviderKind::Codex, &same),
        InstallationKey::derive(ProviderKind::Codex, &other_home),
        InstallationKey::derive(ProviderKind::Codex, &other_launcher),
    ];

    let unique: HashSet<_> = identities.iter().map(|identity| &identity.key).collect();

    assert_eq!(unique.len(), 3);
    assert_eq!(identities[0].key, identities[1].key);
    assert!(!format!("{:?}", identities[0].key).contains("C:\\A"));
}

#[test]
fn bounded_errors_remove_control_characters_and_credentials() {
    let error = UpdateError::new(
        UpdateErrorKind::ProviderFailed,
        format!("failure\n{}", "x".repeat(10_000)),
    );

    assert!(error.message().len() <= MAX_DIAGNOSTIC_CHARS);
    assert!(!error.message().contains('\n'));
}

#[test]
fn failure_classifier_recognizes_external_locks() {
    let output = ProcessOutput::for_test(
        exit_status_from_code(1),
        String::new(),
        "failed to acquire lock held by another process".into(),
    );

    assert_eq!(
        classify_vendor_failure(ProviderKind::Claude, &output).kind,
        UpdateErrorKind::ExternalLock
    );
}

/// Write an executable stand-in for a vendor CLI. The extension and the
/// execute bit are what make a script runnable on each platform, so the
/// caller supplies only the script body for its own shell.
fn fake_launcher(name: &str, body: &str) -> (PathBuf, PathBuf) {
    let root = env::temp_dir().join(format!(
        "NiumaTerm provider update {} {}",
        name,
        process::id()
    ));

    fs::create_dir_all(&root).unwrap();

    #[cfg(windows)]
    let launcher = root.join(format!("{name}.cmd"));

    #[cfg(unix)]
    let launcher = root.join(name);

    fs::write(&launcher, body).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755)).unwrap();
    }

    (root, launcher)
}

#[test]
fn configured_vendor_runners_pass_only_the_allowlisted_update_argument() {
    #[cfg(windows)]
    let script = "@echo off\r\n>\"%NMT_UPDATE_LOG%\" echo %*\r\nif \"%1\"==\"update\" exit /b 0\r\nexit /b 9\r\n";

    #[cfg(unix)]
    let script =
        "#!/bin/sh\necho \"$@\" > \"$NMT_UPDATE_LOG\"\n[ \"$1\" = update ] && exit 0\nexit 9\n";

    for (provider, name) in [
        (ProviderKind::Codex, "fake-codex"),
        (ProviderKind::Claude, "fake-claude"),
    ] {
        let (root, executable) = fake_launcher(name, script);
        let log = root.join("arguments.txt");

        let launcher = AgentCli::new(
            executable.display().to_string(),
            [("NMT_UPDATE_LOG".to_string(), log.display().to_string())],
        );

        nmt_platform::runtime()
            .block_on(vendor_update(&launcher, provider))
            .unwrap();

        assert_eq!(fs::read_to_string(&log).unwrap().trim(), "update");

        let _ = fs::remove_dir_all(root);
    }
}

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

    fn probe<'a>(&'a self, _: &'a AgentCli) -> BoxFuture<'a, Result<VersionStatus, UpdateError>> {
        ready(Ok(VersionStatus {
            provider: ProviderKind::Codex,
            current: Some(Version::new(1, 0, 0)),
            available: Some(Version::new(1, 1, 0)),
            install_method: Some("fake".into()),
            channel: None,
            can_update: true,
            support: DiscoverySupport::Supported,
            remediation: None,
        }))
        .boxed()
    }

    fn update<'a>(&'a self, _: &'a AgentCli) -> BoxFuture<'a, Result<String, UpdateError>> {
        ready(Err(UpdateError::new(
            UpdateErrorKind::ExternalLock,
            "provider files are locked",
        )))
        .boxed()
    }
}

impl ProviderMaintenance for FakeMaintenance {
    fn provider(&self) -> ProviderKind {
        self.provider
    }

    fn probe<'a>(&'a self, _: &'a AgentCli) -> BoxFuture<'a, Result<VersionStatus, UpdateError>> {
        self.probes.fetch_add(1, Ordering::SeqCst);

        ready(Ok(VersionStatus {
            provider: self.provider,
            current: Some(Version::new(1, 0, 0)),
            available: Some(Version::new(1, 1, 0)),
            install_method: Some("fake".into()),
            channel: Some("latest".into()),
            can_update: true,
            support: DiscoverySupport::Supported,
            remediation: Some("do-not-cache-provider-command".into()),
        }))
        .boxed()
    }

    fn update<'a>(&'a self, _: &'a AgentCli) -> BoxFuture<'a, Result<String, UpdateError>> {
        self.updates.fetch_add(1, Ordering::SeqCst);

        ready(Ok("updated".into())).boxed()
    }
}

fn test_path(name: &str) -> PathBuf {
    env::temp_dir().join(format!("niumaterm-update-{name}-{}.json", process::id()))
}

#[test]
fn waiting_for_cache_persistence_keeps_update_state_readable() {
    let path = test_path("pending-write");
    let coordinator = UpdateCoordinator::new(path.clone());

    let key = coordinator.register(
        ProviderKind::Codex,
        AgentCli::new("fake-codex", []),
        Arc::new(FakeMaintenance {
            provider: ProviderKind::Codex,
            probes: AtomicUsize::new(0),
            updates: AtomicUsize::new(0),
        }),
    );

    let disk_busy = coordinator.cache_write.lock();
    let checker = coordinator.clone();
    let checked_key = key.clone();

    let worker =
        thread::spawn(move || nmt_platform::runtime().block_on(checker.check(&checked_key, true)));

    let deadline = Instant::now() + Duration::from_secs(3);

    let mut available = false;

    while Instant::now() < deadline {
        if let Some(inner) = coordinator.inner.try_lock()
            && inner.records[&key].state.phase == UpdatePhase::Available
        {
            available = true;

            break;
        }

        thread::sleep(Duration::from_millis(1));
    }

    drop(disk_busy);

    worker.join().unwrap().unwrap();

    assert!(
        available,
        "state must remain readable while persistence waits"
    );
    assert!(read_cache(&path).installations.contains_key(key.as_str()));

    fs::remove_file(path).unwrap();
}

#[test]
fn dismissing_updates_returns_while_the_cache_writer_is_busy() {
    let path = test_path("dismiss-pending-write");
    let coordinator = UpdateCoordinator::new(path.clone());

    let key = coordinator.register(
        ProviderKind::Codex,
        AgentCli::new("fake-codex", []),
        Arc::new(FakeMaintenance {
            provider: ProviderKind::Codex,
            probes: AtomicUsize::new(0),
            updates: AtomicUsize::new(0),
        }),
    );

    nmt_platform::runtime()
        .block_on(coordinator.check(&key, true))
        .unwrap();

    let disk_busy = coordinator.cache_write.lock();
    let worker = coordinator.clone();
    let target = Version::new(1, 2, 0);
    let updated_key = key.clone();
    let updated_target = target.clone();
    let (completed, received) = mpsc::channel();

    let task = thread::spawn(move || {
        worker.dismiss_available(&updated_key, &Version::new(1, 1, 0));
        worker.dismiss_available(&updated_key, &updated_target);
        completed.send(()).unwrap();
    });

    let returned = received.recv_timeout(Duration::from_secs(3));

    drop(disk_busy);

    task.join().unwrap();

    assert!(
        returned.is_ok(),
        "dismissal must not wait for the filesystem"
    );

    nmt_platform::runtime()
        .block_on(coordinator.persist_cache())
        .unwrap();

    assert_eq!(
        read_cache(&path).installations[key.as_str()].dismissed_target,
        Some(target)
    );

    fs::remove_file(path).unwrap();
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

    nmt_platform::runtime()
        .block_on(coordinator.check(&key, false))
        .unwrap();

    nmt_platform::runtime()
        .block_on(coordinator.check(&key, false))
        .unwrap();

    assert_eq!(fake.probes.load(Ordering::SeqCst), 1);
    assert!(
        !fs::read_to_string(&path)
            .unwrap()
            .contains("do-not-cache-provider-command")
    );

    nmt_platform::runtime()
        .block_on(coordinator.check(&key, true))
        .unwrap();

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

    nmt_platform::runtime()
        .block_on(coordinator.check(&key, true))
        .unwrap();

    coordinator.begin_update(&key).unwrap();

    assert!(coordinator.begin_update(&key).is_err());

    nmt_platform::runtime()
        .block_on(coordinator.run_vendor_update(&key))
        .unwrap();

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

    let available = nmt_platform::runtime()
        .block_on(coordinator.check(&key, true))
        .unwrap();

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

    nmt_platform::runtime()
        .block_on(coordinator.check(&key, true))
        .unwrap();

    coordinator.begin_update(&key).unwrap();

    let error = nmt_platform::runtime()
        .block_on(coordinator.run_vendor_update(&key))
        .unwrap_err();

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
