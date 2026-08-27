use serde_json::Value;

use crate::launcher::{AgentCli, run_bounded};
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

    fn probe(&self, launcher: &AgentCli) -> Result<VersionStatus, UpdateError> {
        match run_bounded(launcher, ["doctor", "--json"], PROBE_LIMITS) {
            Ok(output) => match parse_codex_doctor(output.stdout_for_parsing()) {
                Ok(status) => Ok(status),
                Err(doctor_error) => version_fallback(launcher, doctor_error.message()),
            },
            Err(error) => version_fallback(launcher, &error.to_string()),
        }
    }

    fn update(&self, launcher: &AgentCli) -> Result<VendorUpdateResult, UpdateError> {
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

fn version_fallback(launcher: &AgentCli, reason: &str) -> Result<VersionStatus, UpdateError> {
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
mod tests;
