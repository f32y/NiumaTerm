use std::path::PathBuf;
use std::{env, fs, process};

use semver::Version;

use crate::codex::update::*;

const CODEX_DOCTOR: &str = r#"{
  "schemaVersion": 1,
  "codexVersion": "0.146.0",
  "checks": {
    "runtime.provenance": {"details": {"install method": "npm (package C:\\\\codex)"}},
    "installation": {"details": {"install context": "npm"}},
    "updates.status": {"details": {
      "latest version": "0.147.0",
      "update action": "npm install -g @openai/codex"
    }}
  }
}"#;

#[test]
fn doctor_schema_is_validated_and_remediation_is_display_only() {
    let status = parse_codex_doctor(CODEX_DOCTOR).unwrap();
    assert_eq!(status.current, Some(Version::new(0, 146, 0)));
    assert_eq!(status.available, Some(Version::new(0, 147, 0)));
    assert!(status.update_available());
    assert!(status.can_update);
    assert_eq!(
        status.remediation.as_deref(),
        Some("npm install -g @openai/codex")
    );

    let unsupported = CODEX_DOCTOR.replacen("\"schemaVersion\": 1", "\"schemaVersion\": 2", 1);
    assert_eq!(
        parse_codex_doctor(&unsupported).unwrap_err().kind,
        UpdateErrorKind::InvalidResponse
    );
    let invalid_version = CODEX_DOCTOR.replacen("0.147.0", "latest", 1);
    assert!(parse_codex_doctor(&invalid_version).is_err());
}

#[test]
fn version_fallback_uses_the_same_configured_launcher() {
    let script = "@echo off\r\nif \"%1\"==\"--version\" (echo configured-cli 9.8.7 & exit /b 0)\r\nexit /b 7\r\n";
    let (root, executable) = fake_launcher("version fallback", script);
    let launcher = AgentCli::new(executable.display().to_string(), []);

    let status = CodexMaintenance.probe(&launcher).unwrap();
    assert_eq!(status.current, Some(Version::new(9, 8, 7)));
    assert!(matches!(
        status.support,
        DiscoverySupport::Unsupported { .. }
    ));
    let _ = fs::remove_dir_all(root);
}

fn fake_launcher(name: &str, body: &str) -> (PathBuf, PathBuf) {
    let root = env::temp_dir().join(format!("NiumaTerm Codex update {} {}", name, process::id()));
    fs::create_dir_all(&root).unwrap();
    let launcher = root.join(format!("{name}.cmd"));
    fs::write(&launcher, body).unwrap();
    (root, launcher)
}
