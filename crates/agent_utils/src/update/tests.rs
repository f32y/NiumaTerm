use std::collections::HashSet;
use std::{env, fs, process};

use nmt_platform::windows::process::exit_status_from_code;

use crate::update::*;

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

fn fake_launcher(name: &str, body: &str) -> (PathBuf, PathBuf) {
    let root = env::temp_dir().join(format!(
        "NiumaTerm provider update {} {}",
        name,
        process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    let launcher = root.join(format!("{name}.cmd"));
    fs::write(&launcher, body).unwrap();
    (root, launcher)
}

#[test]
fn configured_vendor_runners_pass_only_the_allowlisted_update_argument() {
    let script = "@echo off\r\n>\"%NMT_UPDATE_LOG%\" echo %*\r\nif \"%1\"==\"update\" exit /b 0\r\nexit /b 9\r\n";
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

        vendor_update(&launcher, provider).unwrap();
        assert_eq!(fs::read_to_string(&log).unwrap().trim(), "update");
        let _ = fs::remove_dir_all(root);
    }
}
