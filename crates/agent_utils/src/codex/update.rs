use serde_json::Value;

use crate::launcher::{ConfiguredLauncher, run_bounded};
use crate::update::{
    DiscoverySupport, MAX_LABEL_CHARS, PROBE_LIMITS, ProviderKind, ProviderMaintenance,
    UpdateError, UpdateErrorKind, VendorUpdateResult, VersionStatus, bounded_label,
    current_version_fallback, parse_strict_version, vendor_update,
};

#[derive(Default)]
pub struct CodexMaintenance;

impl ProviderMaintenance for CodexMaintenance {
    fn provider(&self) -> ProviderKind {
        ProviderKind::Codex
    }

    fn probe(&self, launcher: &ConfiguredLauncher) -> Result<VersionStatus, UpdateError> {
        match run_bounded(launcher, ["doctor", "--json"], PROBE_LIMITS) {
            Ok(output) => match parse_codex_doctor(output.stdout_for_parsing()) {
                Ok(status) => Ok(status),
                Err(doctor_error) => version_fallback(launcher, doctor_error.message()),
            },
            Err(error) => version_fallback(launcher, &error.to_string()),
        }
    }

    fn update(&self, launcher: &ConfiguredLauncher) -> Result<VendorUpdateResult, UpdateError> {
        vendor_update(launcher, ProviderKind::Codex)
    }
}

pub fn parse_codex_doctor(json: &str) -> Result<VersionStatus, UpdateError> {
    let report: Value = serde_json::from_str(json).map_err(|_| {
        UpdateError::new(
            UpdateErrorKind::InvalidResponse,
            "Codex doctor did not return valid JSON",
        )
    })?;
    if report["schemaVersion"].as_u64() != Some(1) || !report["checks"].is_object() {
        return Err(UpdateError::new(
            UpdateErrorKind::InvalidResponse,
            "Codex doctor returned an unsupported schema",
        ));
    }

    let current = report["codexVersion"]
        .as_str()
        .map(|value| parse_strict_version(value, "Codex version"))
        .transpose()?;
    let updates = &report["checks"]["updates.status"];
    let details = &updates["details"];
    let available = detail_string(details, "latest version")
        .map(|value| parse_strict_version(value, "Codex latest version"))
        .transpose()?;
    let install_method = detail_string(
        &report["checks"]["runtime.provenance"]["details"],
        "install method",
    )
    .or_else(|| {
        detail_string(
            &report["checks"]["installation"]["details"],
            "install context",
        )
    })
    .map(|value| bounded_label(value, MAX_LABEL_CHARS));
    let remediation =
        detail_string(details, "update action").map(|value| bounded_label(value, MAX_LABEL_CHARS));
    let can_update = remediation.as_deref().is_some_and(|action| {
        !action.to_ascii_lowercase().contains("manual")
            && !action.to_ascii_lowercase().contains("unknown")
    });
    let support = if available.is_some() {
        DiscoverySupport::Supported
    } else {
        DiscoverySupport::Unsupported {
            reason: "Codex doctor did not publish an available version".to_string(),
        }
    };

    Ok(VersionStatus {
        provider: ProviderKind::Codex,
        current,
        available,
        install_method,
        channel: None,
        can_update,
        support,
        remediation,
    })
}

fn version_fallback(
    launcher: &ConfiguredLauncher,
    reason: &str,
) -> Result<VersionStatus, UpdateError> {
    Ok(VersionStatus {
        provider: ProviderKind::Codex,
        current: current_version_fallback(launcher),
        available: None,
        install_method: None,
        channel: None,
        can_update: false,
        support: DiscoverySupport::Unsupported {
            reason: bounded_label(reason, MAX_LABEL_CHARS),
        },
        remediation: None,
    })
}

fn detail_string<'a>(details: &'a Value, key: &str) -> Option<&'a str> {
    details.as_object()?.get(key)?.as_str()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::{env, fs, process};

    use semver::Version;

    use super::*;

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
        let launcher = ConfiguredLauncher::new(executable.display().to_string(), []);

        let status = CodexMaintenance.probe(&launcher).unwrap();
        assert_eq!(status.current, Some(Version::new(9, 8, 7)));
        assert!(matches!(
            status.support,
            DiscoverySupport::Unsupported { .. }
        ));
        let _ = fs::remove_dir_all(root);
    }

    fn fake_launcher(name: &str, body: &str) -> (PathBuf, PathBuf) {
        let root =
            env::temp_dir().join(format!("NiumaTerm Codex update {} {}", name, process::id()));
        fs::create_dir_all(&root).unwrap();
        let launcher = root.join(format!("{name}.cmd"));
        fs::write(&launcher, body).unwrap();
        (root, launcher)
    }
}
