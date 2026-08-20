use std::time::Duration;

use nmt_agent_utils::update::{InstallationKey, InstallationSnapshot, ProviderKind, UpdatePhase};
use nmt_i18n::i18n;
use semver::Version;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpdateNotificationTone {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotificationPrimaryAction {
    Update,
    Retry,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NotificationProgress {
    None,
    Indeterminate,
    Determinate(f32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FocusedVisibleLifetime {
    phase: UpdatePhase,
    elapsed: Duration,
}

impl FocusedVisibleLifetime {
    pub(crate) fn new(phase: UpdatePhase) -> Self {
        Self {
            phase,
            elapsed: Duration::ZERO,
        }
    }

    pub(crate) fn set_phase(&mut self, phase: UpdatePhase) {
        if self.phase != phase {
            self.phase = phase;
            self.elapsed = Duration::ZERO;
        }
    }

    pub(crate) fn tick(&mut self, focused_visible: bool, elapsed: Duration) -> bool {
        if focused_visible {
            self.elapsed = self.elapsed.saturating_add(elapsed);
        }
        self.elapsed >= Duration::from_secs(3)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UpdateNotificationView {
    pub key: String,
    pub installation: InstallationKey,
    pub provider: ProviderKind,
    pub phase: UpdatePhase,
    pub target: Option<Version>,
    pub title: String,
    pub message: String,
    pub tone: UpdateNotificationTone,
    pub primary: Option<NotificationPrimaryAction>,
    pub show_settings: bool,
    pub progress: NotificationProgress,
    pub terminal_timeout: bool,
}

/// Pure mapping from authoritative coordinator state to one stable card view.
/// The identity retains installation plus target version across every phase.
pub(crate) fn notification_view(snapshot: &InstallationSnapshot) -> Option<UpdateNotificationView> {
    let versions = snapshot.state.versions.as_ref();
    let current = versions
        .and_then(|status| status.current.as_ref())
        .map(ToString::to_string)
        .unwrap_or_else(|| i18n("agent-update-version-unknown").to_string());
    let target = versions.and_then(|status| status.available.clone());
    let target_text = target
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| i18n("agent-update-version-unknown").to_string());
    let phase = snapshot.state.phase;

    if snapshot.notification_hidden
        || phase == UpdatePhase::Available
            && target
                .as_ref()
                .is_some_and(|target| snapshot.dismissed_target.as_ref() == Some(target))
        || matches!(
            phase,
            UpdatePhase::Unknown
                | UpdatePhase::Checking
                | UpdatePhase::Current
                | UpdatePhase::Unsupported
        )
    {
        return None;
    }

    let provider = snapshot.identity.provider.display();
    let (title, message, tone, primary, progress, terminal_timeout) = match phase {
        UpdatePhase::Available => (
            i18n("agent-update-notice-available-title").replace("{provider}", provider),
            format!("{current} → {target_text}"),
            UpdateNotificationTone::Info,
            versions
                .is_some_and(|status| status.can_update)
                .then_some(NotificationPrimaryAction::Update),
            NotificationProgress::None,
            false,
        ),
        UpdatePhase::WaitingForIdle => (
            i18n("agent-update-notice-waiting-title").replace("{provider}", provider),
            i18n("agent-update-notice-waiting-message").to_string(),
            UpdateNotificationTone::Info,
            None,
            NotificationProgress::Indeterminate,
            false,
        ),
        UpdatePhase::Suspending => (
            i18n("agent-update-notice-stopping-title").replace("{provider}", provider),
            i18n("agent-update-notice-stopping-message").to_string(),
            UpdateNotificationTone::Info,
            None,
            progress_view(snapshot),
            false,
        ),
        UpdatePhase::Updating => (
            i18n("agent-update-notice-updating-title").replace("{provider}", provider),
            i18n("agent-update-notice-updating-message").replace("{target}", &target_text),
            UpdateNotificationTone::Info,
            None,
            NotificationProgress::Indeterminate,
            false,
        ),
        UpdatePhase::Verifying => (
            i18n("agent-update-notice-verifying-title").replace("{provider}", provider),
            i18n("agent-update-notice-verifying-message").to_string(),
            UpdateNotificationTone::Info,
            None,
            NotificationProgress::Indeterminate,
            false,
        ),
        UpdatePhase::Restoring => (
            i18n("agent-update-notice-restoring-title").replace("{provider}", provider),
            i18n("agent-update-notice-restoring-message").to_string(),
            UpdateNotificationTone::Info,
            None,
            progress_view(snapshot),
            false,
        ),
        UpdatePhase::Updated => (
            i18n("agent-update-notice-updated-title").replace("{provider}", provider),
            i18n("agent-update-notice-updated-message").replace("{version}", &current),
            UpdateNotificationTone::Success,
            None,
            NotificationProgress::Determinate(100.0),
            true,
        ),
        UpdatePhase::Unchanged => (
            i18n("agent-update-notice-unchanged-title").replace("{provider}", provider),
            bounded_error(snapshot, i18n("agent-update-notice-unchanged-message")),
            UpdateNotificationTone::Warning,
            Some(NotificationPrimaryAction::Retry),
            NotificationProgress::Determinate(100.0),
            true,
        ),
        UpdatePhase::Failed => (
            i18n("agent-update-notice-failed-title").replace("{provider}", provider),
            bounded_error(snapshot, i18n("agent-update-notice-failed-message")),
            UpdateNotificationTone::Error,
            Some(NotificationPrimaryAction::Retry),
            NotificationProgress::None,
            false,
        ),
        _ => return None,
    };

    let identity_target = target
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| "unknown".to_string());
    Some(UpdateNotificationView {
        key: format!("{}:{identity_target}", snapshot.identity.key),
        installation: snapshot.identity.key.clone(),
        provider: snapshot.identity.provider,
        phase,
        target,
        title,
        message,
        tone,
        primary,
        show_settings: true,
        progress,
        terminal_timeout,
    })
}

fn progress_view(snapshot: &InstallationSnapshot) -> NotificationProgress {
    snapshot
        .state
        .progress
        .map_or(NotificationProgress::Indeterminate, |progress| {
            if progress.total == 0 {
                NotificationProgress::Indeterminate
            } else {
                NotificationProgress::Determinate(
                    progress.completed as f32 * 100.0 / progress.total as f32,
                )
            }
        })
}

fn bounded_error(snapshot: &InstallationSnapshot, fallback: &str) -> String {
    snapshot
        .state
        .error
        .as_ref()
        .map(|error| error.message().chars().take(512).collect())
        .unwrap_or_else(|| fallback.to_string())
}
