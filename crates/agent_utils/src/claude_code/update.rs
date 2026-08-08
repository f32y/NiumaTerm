use std::time::Duration;

use reqwest::blocking::{Client, Response};
use semver::Version;

use crate::launcher::{AgentCli, run_bounded};
use crate::update::{
    DiscoverySupport, MAX_LABEL_CHARS, PROBE_LIMITS, ProviderKind, ProviderMaintenance,
    UpdateError, UpdateErrorKind, VendorUpdateResult, VersionStatus, bounded_label,
    current_version_fallback, parse_strict_version, vendor_update,
};

const RELEASE_BASE_URL: &str = "https://downloads.claude.ai/claude-code-releases";

pub trait ClaudeReleaseChannel: Send + Sync {
    fn latest(&self, channel: &str) -> Result<Version, UpdateError>;
}

pub struct HttpClaudeReleaseChannel {
    client: Client,
    base_url: String,
}

impl HttpClaudeReleaseChannel {
    pub fn new() -> Result<Self, UpdateError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| {
                UpdateError::new(
                    UpdateErrorKind::Network,
                    "could not initialize Claude release client",
                )
            })?;
        Ok(Self {
            client,
            base_url: RELEASE_BASE_URL.to_string(),
        })
    }

    #[cfg(test)]
    fn with_base_url(base_url: impl Into<String>) -> Result<Self, UpdateError> {
        Self::new().map(|mut client| {
            client.base_url = base_url.into();
            client
        })
    }
}

impl ClaudeReleaseChannel for HttpClaudeReleaseChannel {
    fn latest(&self, channel: &str) -> Result<Version, UpdateError> {
        if !matches!(channel, "latest" | "stable") {
            return Err(UpdateError::new(
                UpdateErrorKind::Unsupported,
                format!("Claude release channel `{channel}` is not supported"),
            ));
        }
        let response = self
            .client
            .get(format!("{}/{channel}", self.base_url.trim_end_matches('/')))
            .send()
            .and_then(Response::error_for_status)
            .map_err(|_| {
                UpdateError::new(
                    UpdateErrorKind::Network,
                    "Claude release service request failed",
                )
            })?;
        let content_length = response.content_length().unwrap_or(0);
        if content_length > 256 {
            return Err(UpdateError::new(
                UpdateErrorKind::InvalidResponse,
                "Claude release response exceeded the version limit",
            ));
        }
        let body = response.text().map_err(|_| {
            UpdateError::new(
                UpdateErrorKind::InvalidResponse,
                "could not read Claude release response",
            )
        })?;
        parse_strict_version(body.trim(), "Claude release version")
    }
}

pub struct ClaudeMaintenance<C> {
    releases: C,
}

impl<C> ClaudeMaintenance<C> {
    pub fn new(releases: C) -> Self {
        Self { releases }
    }
}

impl<C> ProviderMaintenance for ClaudeMaintenance<C>
where
    C: ClaudeReleaseChannel,
{
    fn provider(&self) -> ProviderKind {
        ProviderKind::Claude
    }

    fn probe(&self, launcher: &AgentCli) -> Result<VersionStatus, UpdateError> {
        let doctor = run_bounded(launcher, ["doctor"], PROBE_LIMITS);
        let mut status = match doctor {
            Ok(output) => {
                parse_claude_doctor(output.stdout_for_parsing()).unwrap_or_else(|error| {
                    VersionStatus {
                        provider: ProviderKind::Claude,
                        current: None,
                        available: None,
                        install_method: None,
                        channel: None,
                        can_update: false,
                        support: DiscoverySupport::Unsupported {
                            reason: error.message().to_string(),
                        },
                        remediation: None,
                    }
                })
            }
            Err(error) => VersionStatus {
                provider: ProviderKind::Claude,
                current: None,
                available: None,
                install_method: None,
                channel: None,
                can_update: false,
                support: DiscoverySupport::Unsupported {
                    reason: bounded_label(&error.to_string(), MAX_LABEL_CHARS),
                },
                remediation: None,
            },
        };

        if status.current.is_none() {
            status.current = current_version_fallback(launcher);
        }

        let Some(channel) = status.channel.as_deref() else {
            return Ok(status);
        };
        if !matches!(channel, "latest" | "stable") {
            status.support = DiscoverySupport::Unsupported {
                reason: format!("Claude release channel `{channel}` is not supported"),
            };
            status.can_update = false;
            return Ok(status);
        }

        match self.releases.latest(channel) {
            Ok(version) => {
                status.available = Some(version);
                status.support = DiscoverySupport::Supported;
                Ok(status)
            }
            Err(error) => {
                status.support = DiscoverySupport::Unsupported {
                    reason: error.message().to_string(),
                };
                Ok(status)
            }
        }
    }

    fn update(&self, launcher: &AgentCli) -> Result<VendorUpdateResult, UpdateError> {
        vendor_update(launcher, ProviderKind::Claude)
    }
}

pub fn parse_claude_doctor(output: &str) -> Result<VersionStatus, UpdateError> {
    let mut current = None;
    let mut running_method = None;
    let mut configured_method = None;
    let mut channel = None;
    let mut auto_updates = None;

    for line in output.lines().take(256) {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("Running:") {
            let value = value.trim();
            if let (Some(open), Some(close)) = (value.rfind('('), value.rfind(')'))
                && open < close
            {
                running_method = nonempty_label(&value[..open]);
                current = Some(parse_strict_version(
                    value[open + 1..close].trim(),
                    "Claude version",
                )?);
            }
        } else if let Some(value) = line.strip_prefix("Config install method:") {
            configured_method = nonempty_label(value);
        } else if let Some(value) = line.strip_prefix("Auto-update channel:") {
            channel = nonempty_label(value).map(|value| value.to_ascii_lowercase());
        } else if let Some(value) = line.strip_prefix("Auto-updates:") {
            auto_updates = nonempty_label(value).map(|value| value.to_ascii_lowercase());
        }
    }

    if current.is_none() && running_method.is_none() && configured_method.is_none() {
        return Err(UpdateError::new(
            UpdateErrorKind::InvalidResponse,
            "Claude doctor output did not contain installation metadata",
        ));
    }

    let install_method = configured_method.or(running_method);
    let supported_install = install_method.as_deref().is_some_and(|method| {
        ["native", "npm", "homebrew", "brew"]
            .iter()
            .any(|known| method.to_ascii_lowercase().contains(known))
    });
    let update_configuration = auto_updates.map(|value| format!("auto-updates {value}"));

    Ok(VersionStatus {
        provider: ProviderKind::Claude,
        current,
        available: None,
        install_method,
        channel,
        can_update: supported_install,
        support: DiscoverySupport::Unsupported {
            reason: "Claude release version has not been checked".to_string(),
        },
        remediation: update_configuration,
    })
}

fn nonempty_label(value: &str) -> Option<String> {
    let value = bounded_label(value, MAX_LABEL_CHARS);
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE_DOCTOR: &str = "Claude Code doctor\n\nRunning: native (2.1.222)\nConfig install method: native\nAuto-updates: enabled\nAuto-update channel: latest\n";

    #[test]
    fn doctor_parses_bounded_known_fields() {
        let status = parse_claude_doctor(CLAUDE_DOCTOR).unwrap();
        assert_eq!(status.current, Some(Version::new(2, 1, 222)));
        assert_eq!(status.install_method.as_deref(), Some("native"));
        assert_eq!(status.channel.as_deref(), Some("latest"));
        assert!(status.can_update);

        let unknown = CLAUDE_DOCTOR.replace("latest", "canary");
        let status = parse_claude_doctor(&unknown).unwrap();
        assert_eq!(status.channel.as_deref(), Some("canary"));
    }

    struct FakeReleases(Result<Version, UpdateError>);

    impl ClaudeReleaseChannel for FakeReleases {
        fn latest(&self, _channel: &str) -> Result<Version, UpdateError> {
            self.0.clone()
        }
    }

    #[test]
    fn release_client_contract_rejects_unknown_channels_and_invalid_versions() {
        let client = HttpClaudeReleaseChannel::with_base_url("http://127.0.0.1:9").unwrap();
        assert_eq!(
            client.latest("canary").unwrap_err().kind,
            UpdateErrorKind::Unsupported
        );
        assert!(parse_strict_version("2.1.222", "version").is_ok());
        assert!(parse_strict_version("v2.1.222", "version").is_err());
        let fake = FakeReleases(Ok(Version::new(2, 1, 223)));
        assert_eq!(fake.latest("latest").unwrap(), Version::new(2, 1, 223));
    }
}
