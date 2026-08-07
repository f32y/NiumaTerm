//! Provider-neutral discovery, update coordination, caching, and vendor-managed adapters.

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use std::{fmt, fs};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use parking_lot::Mutex;
use reqwest::blocking::{Client, Response};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::launcher::{
    ConfiguredLauncher, ProcessError, ProcessLimits, ProcessOutput, run_bounded,
};

const PROBE_LIMITS: ProcessLimits = ProcessLimits::new(Duration::from_secs(30), 256 * 1024);
const UPDATE_LIMITS: ProcessLimits = ProcessLimits::new(Duration::from_secs(15 * 60), 256 * 1024);
const MAX_LABEL_CHARS: usize = 160;
const MAX_DIAGNOSTIC_CHARS: usize = 4_096;
const CLAUDE_RELEASE_BASE_URL: &str = "https://downloads.claude.ai/claude-code-releases";

const UPDATE_ENVIRONMENT_NAMES: [&str; 10] = [
    "PATH",
    "CODEX_HOME",
    "CLAUDE_CONFIG_DIR",
    "NPM_CONFIG_PREFIX",
    "npm_config_prefix",
    "BUN_INSTALL",
    "PNPM_HOME",
    "CODEX_MANAGED_PACKAGE_ROOT",
    "CODEX_MANAGED_BY_BUN",
    "CODEX_MANAGED_BY_PNPM",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Claude,
    Codex,
}

impl ProviderKind {
    pub const fn display(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    pub const fn default_executable(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// Stable, opaque identity for one effective provider installation. Debug and
/// display output expose only the provider and irreversible digest.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InstallationKey(String);

impl InstallationKey {
    pub fn derive(provider: ProviderKind, launcher: &ConfiguredLauncher) -> InstallationIdentity {
        let resolved_launcher = launcher.resolved_executable();
        let mut digest = Sha256::new();
        digest.update(match provider {
            ProviderKind::Claude => b"claude\0".as_slice(),
            ProviderKind::Codex => b"codex\0".as_slice(),
        });
        digest.update(
            resolved_launcher
                .to_string_lossy()
                .to_ascii_lowercase()
                .as_bytes(),
        );
        digest.update([0]);

        for name in UPDATE_ENVIRONMENT_NAMES {
            digest.update(name.to_ascii_uppercase().as_bytes());
            digest.update([b'=']);
            if let Some(value) = launcher.effective_env_os(name) {
                digest.update(value.to_string_lossy().as_bytes());
            }
            digest.update([0]);
        }

        let fingerprint = hex_digest(digest.finalize().as_slice());
        let key = Self(format!(
            "{}:{}",
            match provider {
                ProviderKind::Claude => "claude",
                ProviderKind::Codex => "codex",
            },
            fingerprint
        ));
        InstallationIdentity {
            key,
            provider,
            resolved_launcher,
            environment_fingerprint: fingerprint,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for InstallationKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("InstallationKey")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for InstallationKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallationIdentity {
    pub key: InstallationKey,
    pub provider: ProviderKind,
    pub resolved_launcher: PathBuf,
    pub environment_fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum DiscoverySupport {
    Supported,
    Unsupported { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionStatus {
    pub provider: ProviderKind,
    pub current: Option<Version>,
    pub available: Option<Version>,
    pub install_method: Option<String>,
    pub channel: Option<String>,
    pub can_update: bool,
    pub support: DiscoverySupport,
    /// Vendor remediation is presentation-only and is never interpreted as a
    /// shell command.
    pub remediation: Option<String>,
}

impl VersionStatus {
    pub fn update_available(&self) -> bool {
        matches!((&self.current, &self.available), (Some(current), Some(available)) if available > current)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePhase {
    #[default]
    Unknown,
    Checking,
    Current,
    Available,
    WaitingForIdle,
    Suspending,
    Updating,
    Verifying,
    Restoring,
    Updated,
    Unchanged,
    Unsupported,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateProgress {
    pub completed: usize,
    pub total: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallationUpdateState {
    pub phase: UpdatePhase,
    pub versions: Option<VersionStatus>,
    pub progress: Option<UpdateProgress>,
    pub error: Option<UpdateError>,
}

impl Default for InstallationUpdateState {
    fn default() -> Self {
        Self {
            phase: UpdatePhase::Unknown,
            versions: None,
            progress: None,
            error: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateErrorKind {
    Unsupported,
    Launch,
    TimedOut,
    ProviderFailed,
    ExternalLock,
    InvalidResponse,
    Network,
    Recovery,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateError {
    pub kind: UpdateErrorKind,
    message: String,
}

impl UpdateError {
    pub fn new(kind: UpdateErrorKind, message: impl AsRef<str>) -> Self {
        Self {
            kind,
            message: bounded_label(message.as_ref(), MAX_DIAGNOSTIC_CHARS),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for UpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for UpdateError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VendorUpdateResult {
    pub diagnostic: String,
}

pub trait ProviderMaintenance: Send + Sync {
    fn provider(&self) -> ProviderKind;
    fn probe(&self, launcher: &ConfiguredLauncher) -> Result<VersionStatus, UpdateError>;
    fn update(&self, launcher: &ConfiguredLauncher) -> Result<VendorUpdateResult, UpdateError>;
}

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
                Err(doctor_error) => codex_version_fallback(launcher, doctor_error.message()),
            },
            Err(error) => codex_version_fallback(launcher, &error.to_string()),
        }
    }

    fn update(&self, launcher: &ConfiguredLauncher) -> Result<VendorUpdateResult, UpdateError> {
        vendor_update(launcher, ProviderKind::Codex)
    }
}

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
            base_url: CLAUDE_RELEASE_BASE_URL.to_string(),
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

    fn probe(&self, launcher: &ConfiguredLauncher) -> Result<VersionStatus, UpdateError> {
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
            status.current = current_version_fallback(launcher, ProviderKind::Claude);
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

    fn update(&self, launcher: &ConfiguredLauncher) -> Result<VendorUpdateResult, UpdateError> {
        vendor_update(launcher, ProviderKind::Claude)
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

fn codex_version_fallback(
    launcher: &ConfiguredLauncher,
    reason: &str,
) -> Result<VersionStatus, UpdateError> {
    Ok(VersionStatus {
        provider: ProviderKind::Codex,
        current: current_version_fallback(launcher, ProviderKind::Codex),
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

fn current_version_fallback(
    launcher: &ConfiguredLauncher,
    _provider: ProviderKind,
) -> Option<Version> {
    run_bounded(launcher, ["--version"], PROBE_LIMITS)
        .ok()
        .and_then(|output| extract_version(output.stdout_for_parsing()))
}

fn vendor_update(
    launcher: &ConfiguredLauncher,
    provider: ProviderKind,
) -> Result<VendorUpdateResult, UpdateError> {
    let output = run_bounded(launcher, ["update"], UPDATE_LIMITS).map_err(|error| {
        let kind = if matches!(error, ProcessError::TimedOut { .. }) {
            UpdateErrorKind::TimedOut
        } else {
            UpdateErrorKind::Launch
        };
        UpdateError::new(kind, error.to_string())
    })?;
    if !output.success() {
        return Err(classify_vendor_failure(provider, &output));
    }
    Ok(VendorUpdateResult {
        diagnostic: bounded_label(&output.diagnostic(), MAX_DIAGNOSTIC_CHARS),
    })
}

fn classify_vendor_failure(provider: ProviderKind, output: &ProcessOutput) -> UpdateError {
    let diagnostic = output.diagnostic();
    let lower = diagnostic.to_ascii_lowercase();
    let external_lock = [
        "ebusy",
        "eperm",
        "in use",
        "lock held",
        "another process",
        "failed to acquire lock",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    UpdateError::new(
        if external_lock {
            UpdateErrorKind::ExternalLock
        } else {
            UpdateErrorKind::ProviderFailed
        },
        if diagnostic.trim().is_empty() {
            format!("{} update command failed", provider.display())
        } else {
            diagnostic
        },
    )
}

fn detail_string<'a>(details: &'a Value, key: &str) -> Option<&'a str> {
    details.as_object()?.get(key)?.as_str()
}

fn parse_strict_version(value: &str, field: &str) -> Result<Version, UpdateError> {
    Version::parse(value).map_err(|_| {
        UpdateError::new(
            UpdateErrorKind::InvalidResponse,
            format!("{field} was not a strict semantic version"),
        )
    })
}

fn extract_version(output: &str) -> Option<Version> {
    output
        .split_ascii_whitespace()
        .map(|candidate| {
            candidate.trim_matches(|ch: char| {
                !ch.is_ascii_alphanumeric() && ch != '.' && ch != '-' && ch != '+'
            })
        })
        .find_map(|candidate| Version::parse(candidate).ok())
}

fn nonempty_label(value: &str) -> Option<String> {
    let value = bounded_label(value, MAX_LABEL_CHARS);
    (!value.is_empty()).then_some(value)
}

fn bounded_label(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

fn hex_digest(bytes: &[u8]) -> String {
    use fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

const CACHE_VERSION: u32 = 1;
const CACHE_FRESHNESS: ChronoDuration = ChronoDuration::hours(24);

#[derive(Clone, Debug)]
pub struct InstallationSnapshot {
    pub identity: InstallationIdentity,
    pub state: InstallationUpdateState,
    pub last_checked: Option<DateTime<Utc>>,
    pub dismissed_target: Option<Version>,
    pub busy: bool,
    pub notification_hidden: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CacheEntry {
    status: VersionStatus,
    checked_at: DateTime<Utc>,
    dismissed_target: Option<Version>,
}

#[derive(Default, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    installations: HashMap<String, CacheEntry>,
}

struct InstallationRecord {
    identity: InstallationIdentity,
    launcher: ConfiguredLauncher,
    maintenance: Arc<dyn ProviderMaintenance>,
    state: InstallationUpdateState,
    last_checked: Option<DateTime<Utc>>,
    dismissed_target: Option<Version>,
    busy: bool,
    notification_hidden: bool,
}

struct Inner {
    records: HashMap<InstallationKey, InstallationRecord>,
    cache: CacheFile,
}

/// Thread-safe state shared by settings, notification presenters, and the
/// multi-tab transaction. Provider calls happen outside the mutex while the
/// record remains marked busy, so paints can still observe progress.
#[derive(Clone)]
pub struct UpdateCoordinator {
    inner: Arc<Mutex<Inner>>,
    cache_path: PathBuf,
    now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

impl UpdateCoordinator {
    pub fn new(cache_path: PathBuf) -> Self {
        Self::with_clock(cache_path, Arc::new(Utc::now))
    }

    pub fn with_clock(
        cache_path: PathBuf,
        now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                records: HashMap::new(),
                cache: read_cache(&cache_path),
            })),
            cache_path,
            now,
        }
    }

    pub fn register(
        &self,
        provider: ProviderKind,
        launcher: ConfiguredLauncher,
        maintenance: Arc<dyn ProviderMaintenance>,
    ) -> InstallationKey {
        debug_assert_eq!(provider, maintenance.provider());
        let identity = InstallationKey::derive(provider, &launcher);
        let key = identity.key.clone();
        let mut inner = self.inner.lock();
        if inner.records.contains_key(&key) {
            return key;
        }
        let cached = inner.cache.installations.get(key.as_str()).cloned();
        let (state, last_checked, dismissed_target) = cached.map_or_else(
            || (InstallationUpdateState::default(), None, None),
            |entry| {
                (
                    state_from_status(entry.status),
                    Some(entry.checked_at),
                    entry.dismissed_target,
                )
            },
        );
        inner.records.insert(
            key.clone(),
            InstallationRecord {
                identity,
                launcher,
                maintenance,
                state,
                last_checked,
                dismissed_target,
                busy: false,
                notification_hidden: false,
            },
        );
        key
    }

    pub fn snapshots(&self) -> Vec<InstallationSnapshot> {
        let inner = self.inner.lock();
        let mut snapshots = inner
            .records
            .values()
            .map(record_snapshot)
            .collect::<Vec<_>>();
        snapshots.sort_by(|a, b| a.identity.key.as_str().cmp(b.identity.key.as_str()));
        snapshots
    }

    pub fn snapshot(&self, key: &InstallationKey) -> Option<InstallationSnapshot> {
        self.inner.lock().records.get(key).map(record_snapshot)
    }

    /// Check an installation. Automatic callers reuse a successful result for
    /// 24 hours; manual callers always probe the provider.
    pub fn check(&self, key: &InstallationKey, manual: bool) -> Result<VersionStatus, UpdateError> {
        let (launcher, maintenance) = {
            let mut inner = self.inner.lock();
            let record = inner.records.get_mut(key).ok_or_else(|| {
                UpdateError::new(
                    UpdateErrorKind::Unsupported,
                    "unknown provider installation",
                )
            })?;
            if record.busy {
                return Err(UpdateError::new(
                    UpdateErrorKind::ProviderFailed,
                    "an update operation is already running for this installation",
                ));
            }
            let now = (self.now)();
            if !manual
                && let (Some(last_checked), Some(status)) =
                    (record.last_checked, record.state.versions.clone())
                && matches!(status.support, DiscoverySupport::Supported)
                && now.signed_duration_since(last_checked) < CACHE_FRESHNESS
            {
                return Ok(status);
            }
            record.busy = true;
            record.notification_hidden = false;
            record.state.phase = UpdatePhase::Checking;
            record.state.error = None;
            (record.launcher.clone(), record.maintenance.clone())
        };

        let result = maintenance.probe(&launcher);
        let mut inner = self.inner.lock();
        let record = inner.records.get_mut(key).expect("registered installation");
        record.busy = false;
        match &result {
            Ok(status) => {
                let now = (self.now)();
                record.last_checked = Some(now);
                record.state = state_from_status(status.clone());
                if matches!(status.support, DiscoverySupport::Supported) {
                    let entry = CacheEntry {
                        status: cacheable_status(status),
                        checked_at: now,
                        dismissed_target: record.dismissed_target.clone(),
                    };
                    inner.cache.installations.insert(key.to_string(), entry);
                    write_cache(&self.cache_path, &inner.cache);
                }
            }
            Err(error) => {
                record.state.phase = UpdatePhase::Failed;
                record.state.error = Some(error.clone());
            }
        }
        result
    }

    /// Atomically claim the installation before asynchronous update work can
    /// be dispatched, closing the notification double-click window.
    pub fn begin_update(&self, key: &InstallationKey) -> Result<(), UpdateError> {
        let mut inner = self.inner.lock();
        let record = inner.records.get_mut(key).ok_or_else(|| {
            UpdateError::new(
                UpdateErrorKind::Unsupported,
                "unknown provider installation",
            )
        })?;
        if record.busy {
            return Err(UpdateError::new(
                UpdateErrorKind::ProviderFailed,
                "an update operation is already running for this installation",
            ));
        }
        if !record
            .state
            .versions
            .as_ref()
            .is_some_and(VersionStatus::update_available)
        {
            return Err(UpdateError::new(
                UpdateErrorKind::Unsupported,
                "this installation has no verified update available",
            ));
        }
        record.busy = true;
        record.notification_hidden = false;
        record.state.phase = UpdatePhase::WaitingForIdle;
        record.state.progress = None;
        record.state.error = None;
        Ok(())
    }

    pub fn transition(
        &self,
        key: &InstallationKey,
        phase: UpdatePhase,
        progress: Option<UpdateProgress>,
    ) {
        if let Some(record) = self.inner.lock().records.get_mut(key) {
            record.state.phase = phase;
            record.state.progress = progress;
        }
    }

    pub fn run_vendor_update(
        &self,
        key: &InstallationKey,
    ) -> Result<VendorUpdateResult, UpdateError> {
        let (launcher, maintenance) = self.operation_parts(key)?;
        maintenance.update(&launcher)
    }

    pub fn verify(&self, key: &InstallationKey) -> Result<VersionStatus, UpdateError> {
        let (launcher, maintenance) = self.operation_parts(key)?;
        maintenance.probe(&launcher)
    }

    pub fn finish_update(
        &self,
        key: &InstallationKey,
        verified: Option<VersionStatus>,
        error: Option<UpdateError>,
        restore_failures: usize,
    ) {
        let mut inner = self.inner.lock();
        let Some(record) = inner.records.get_mut(key) else {
            return;
        };
        record.busy = false;
        record.state.progress = None;
        if let Some(status) = verified.as_ref() {
            record.last_checked = Some((self.now)());
            record.state.versions = Some(status.clone());
        }
        if let Some(error) = error {
            record.state.phase = UpdatePhase::Failed;
            record.state.error = Some(error);
            return;
        }
        if restore_failures > 0 {
            record.state.phase = UpdatePhase::Failed;
            record.state.error = Some(UpdateError::new(
                UpdateErrorKind::Recovery,
                format!("{restore_failures} agent tab(s) could not reconnect"),
            ));
            if let Some(status) = verified {
                record.state.versions = Some(status);
            }
            return;
        }
        if let Some(status) = verified {
            let now = (self.now)();
            record.last_checked = Some(now);
            if let DiscoverySupport::Unsupported { reason } = &status.support {
                record.state.phase = UpdatePhase::Failed;
                record.state.error = Some(UpdateError::new(
                    UpdateErrorKind::InvalidResponse,
                    format!("could not verify the installed version: {reason}"),
                ));
                record.state.versions = Some(status);
                return;
            }
            if status.current.is_none() {
                record.state.phase = UpdatePhase::Failed;
                record.state.error = Some(UpdateError::new(
                    UpdateErrorKind::InvalidResponse,
                    "the provider did not publish an installed version after updating",
                ));
                record.state.versions = Some(status);
                return;
            }
            record.state.phase = if status.update_available() {
                UpdatePhase::Unchanged
            } else {
                UpdatePhase::Updated
            };
            record.state.error = (record.state.phase == UpdatePhase::Unchanged).then(|| {
                UpdateError::new(
                    UpdateErrorKind::ProviderFailed,
                    "the provider finished updating but the installed version did not change",
                )
            });
            record.state.versions = Some(status.clone());
            let dismissed_target = record.dismissed_target.clone();
            inner.cache.installations.insert(
                key.to_string(),
                CacheEntry {
                    status: cacheable_status(&status),
                    checked_at: now,
                    dismissed_target,
                },
            );
            write_cache(&self.cache_path, &inner.cache);
        }
    }

    pub fn dismiss_available(&self, key: &InstallationKey, target: &Version) {
        let mut inner = self.inner.lock();
        let Some(record) = inner.records.get_mut(key) else {
            return;
        };
        record.dismissed_target = Some(target.clone());
        if let Some(entry) = inner.cache.installations.get_mut(key.as_str()) {
            entry.dismissed_target = Some(target.clone());
        }
        write_cache(&self.cache_path, &inner.cache);
    }

    pub fn hide_notification(&self, key: &InstallationKey) {
        if let Some(record) = self.inner.lock().records.get_mut(key) {
            record.notification_hidden = true;
        }
    }

    fn operation_parts(
        &self,
        key: &InstallationKey,
    ) -> Result<(ConfiguredLauncher, Arc<dyn ProviderMaintenance>), UpdateError> {
        let inner = self.inner.lock();
        let record = inner.records.get(key).ok_or_else(|| {
            UpdateError::new(
                UpdateErrorKind::Unsupported,
                "unknown provider installation",
            )
        })?;
        if !record.busy {
            return Err(UpdateError::new(
                UpdateErrorKind::ProviderFailed,
                "the installation update was not claimed",
            ));
        }
        Ok((record.launcher.clone(), record.maintenance.clone()))
    }
}

fn state_from_status(status: VersionStatus) -> InstallationUpdateState {
    let phase = if matches!(status.support, DiscoverySupport::Unsupported { .. }) {
        UpdatePhase::Unsupported
    } else if status.update_available() {
        UpdatePhase::Available
    } else {
        UpdatePhase::Current
    };
    InstallationUpdateState {
        phase,
        versions: Some(status),
        progress: None,
        error: None,
    }
}

fn cacheable_status(status: &VersionStatus) -> VersionStatus {
    let mut status = status.clone();
    // Vendor remediation is presentation-only command text. The live probe
    // may show its bounded value, but persistence stores only status metadata.
    status.remediation = None;
    status
}

fn record_snapshot(record: &InstallationRecord) -> InstallationSnapshot {
    InstallationSnapshot {
        identity: record.identity.clone(),
        state: record.state.clone(),
        last_checked: record.last_checked,
        dismissed_target: record.dismissed_target.clone(),
        busy: record.busy,
        notification_hidden: record.notification_hidden,
    }
}

fn read_cache(path: &Path) -> CacheFile {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<CacheFile>(&bytes).ok())
        .filter(|cache| cache.version == CACHE_VERSION)
        .unwrap_or_else(|| CacheFile {
            version: CACHE_VERSION,
            installations: HashMap::new(),
        })
}

fn write_cache(path: &Path, cache: &CacheFile) {
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(bytes) = serde_json::to_vec(cache) else {
        return;
    };
    let temporary = path.with_extension("tmp");
    if fs::write(&temporary, bytes).is_ok() {
        if fs::rename(&temporary, path).is_err() {
            // Windows rename does not replace an existing destination. Cache
            // loss is recoverable by probing again, so a short replacement
            // gap is preferable to leaving every later result stale.
            let _ = fs::remove_file(path);
            let _ = fs::rename(temporary, path);
        }
    }
}

#[cfg(test)]
mod coordinator_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::{env, process};

    use super::*;
    use crate::update::{DiscoverySupport, VendorUpdateResult};

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

        fn probe(&self, _: &ConfiguredLauncher) -> Result<VersionStatus, UpdateError> {
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

        fn update(&self, _: &ConfiguredLauncher) -> Result<VendorUpdateResult, UpdateError> {
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

        fn probe(&self, _: &ConfiguredLauncher) -> Result<VersionStatus, UpdateError> {
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

        fn update(&self, _: &ConfiguredLauncher) -> Result<VendorUpdateResult, UpdateError> {
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
            ConfiguredLauncher::new("fake-codex", []),
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
            ConfiguredLauncher::new("fake-claude", []),
            fake.clone(),
        );
        let duplicate_key = coordinator.register(
            ProviderKind::Claude,
            ConfiguredLauncher::new("fake-claude", []),
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
            ConfiguredLauncher::new("fake-outcomes-claude", []),
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
            ConfiguredLauncher::new("fake-locked-codex", []),
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
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::process::{self, ExitStatus};
    use std::{env, fs};

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

    const CLAUDE_DOCTOR: &str = "Claude Code doctor\n\nRunning: native (2.1.222)\nConfig install method: native\nAuto-updates: enabled\nAuto-update channel: latest\n";

    #[test]
    fn installation_keys_dedupe_shared_launchers_and_split_update_contexts() {
        let first =
            ConfiguredLauncher::new("codex", [("CODEX_HOME".to_string(), "C:\\A".to_string())]);
        let same =
            ConfiguredLauncher::new("codex", [("CODEX_HOME".to_string(), "C:\\A".to_string())]);
        let other_home =
            ConfiguredLauncher::new("codex", [("CODEX_HOME".to_string(), "C:\\B".to_string())]);
        let other_launcher = ConfiguredLauncher::new(
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
    fn codex_doctor_schema_is_validated_and_remediation_is_display_only() {
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
    fn claude_doctor_parses_bounded_known_fields() {
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
        use std::os::windows::process::ExitStatusExt as _;
        let output = ProcessOutput::for_test(
            ExitStatus::from_raw(1),
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
            let launcher = ConfiguredLauncher::new(
                executable.display().to_string(),
                [("NMT_UPDATE_LOG".to_string(), log.display().to_string())],
            );

            vendor_update(&launcher, provider).unwrap();
            assert_eq!(fs::read_to_string(&log).unwrap().trim(), "update");
            let _ = fs::remove_dir_all(root);
        }
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
}
