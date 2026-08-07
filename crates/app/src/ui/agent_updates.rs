//! Provider update registration, test isolation, and presentation reduction.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::{env, process};

use futures::future::join_all;
use gpui::prelude::*;
use gpui::{App, Entity, Global, Window, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::{DialogClose, DialogFooter};
use gpui_component::{ActiveTheme as _, WindowExt as _};
use nmt_agent_utils::launcher::ConfiguredLauncher;
use nmt_agent_utils::update::{
    ClaudeMaintenance, CodexMaintenance, DiscoverySupport, HttpClaudeReleaseChannel,
    InstallationKey, InstallationSnapshot, ProviderKind, ProviderMaintenance, UpdateCoordinator,
    UpdateError, UpdateErrorKind, UpdatePhase, UpdateProgress, VendorUpdateResult, VersionStatus,
};
use semver::Version;

use super::agent_pane::{
    AgentPane, RecoveryReadiness, RecoverySnapshot, RestorationReadiness, agent_launch,
};
use super::settings::{AgentProfile, AgentProfileKind};
use crate::ui::Shell;
use crate::window::ShellRegistry;

pub(crate) struct AgentUpdates {
    pub(crate) coordinator: UpdateCoordinator,
    testing: bool,
    claude: Arc<dyn ProviderMaintenance>,
    codex: Arc<dyn ProviderMaintenance>,
}

impl Global for AgentUpdates {}

impl AgentUpdates {
    fn register_profile(&self, profile: &AgentProfile) -> InstallationKey {
        let provider = provider_for_profile(profile.kind);
        let launch = agent_launch(profile);
        let launcher = ConfiguredLauncher::from_launch(&launch, provider.default_executable());
        let maintenance = match provider {
            ProviderKind::Claude => self.claude.clone(),
            ProviderKind::Codex => self.codex.clone(),
        };
        self.coordinator.register(provider, launcher, maintenance)
    }

    pub(crate) fn testing(&self) -> bool {
        self.testing
    }
}

pub(crate) fn initialize(testing: bool, profiles: &[AgentProfile], cx: &mut App) {
    let cache_path = update_cache_path(testing);

    let (claude, codex): (Arc<dyn ProviderMaintenance>, Arc<dyn ProviderMaintenance>) = if testing {
        (
            Arc::new(FakeMaintenance::new(ProviderKind::Claude)),
            Arc::new(FakeMaintenance::new(ProviderKind::Codex)),
        )
    } else {
        let claude: Arc<dyn ProviderMaintenance> = HttpClaudeReleaseChannel::new().map_or_else(
            |error| {
                Arc::new(UnavailableMaintenance {
                    provider: ProviderKind::Claude,
                    reason: error.to_string(),
                }) as Arc<dyn ProviderMaintenance>
            },
            |releases| Arc::new(ClaudeMaintenance::new(releases)),
        );
        (claude, Arc::new(CodexMaintenance))
    };

    let updates = AgentUpdates {
        coordinator: UpdateCoordinator::new(cache_path),
        testing,
        claude,
        codex,
    };
    for profile in profiles {
        updates.register_profile(profile);
    }
    cx.set_global(updates);
}

fn update_cache_path(testing: bool) -> PathBuf {
    if testing {
        env::temp_dir()
            .join("NiumaTerm")
            .join(format!("update-testing-{}", process::id()))
            .join("agent-update-status.json")
    } else {
        nmt_config::config_dir_path().join("agent-update-status.json")
    }
}

pub(crate) fn reconcile_profiles(profiles: &[AgentProfile], cx: &mut App) {
    let Some(updates) = cx.try_global::<AgentUpdates>() else {
        return;
    };
    for profile in profiles {
        updates.register_profile(profile);
    }
}

fn distinct_installation_keys(
    keys: impl IntoIterator<Item = InstallationKey>,
) -> Vec<InstallationKey> {
    let mut seen = HashSet::new();
    keys.into_iter()
        .filter(|key| seen.insert(key.clone()))
        .collect()
}

pub(crate) fn installations_for_profiles(
    profiles: &[AgentProfile],
    cx: &App,
) -> Vec<InstallationSnapshot> {
    let Some(updates) = cx.try_global::<AgentUpdates>() else {
        return Vec::new();
    };
    distinct_installation_keys(
        profiles
            .iter()
            .map(|profile| updates.register_profile(profile)),
    )
    .into_iter()
    .filter_map(|key| updates.coordinator.snapshot(&key))
    .collect()
}

pub(crate) fn installation(key: &InstallationKey, cx: &App) -> Option<InstallationSnapshot> {
    cx.try_global::<AgentUpdates>()?.coordinator.snapshot(key)
}

pub(crate) fn manual_check_profiles(profiles: &[AgentProfile], cx: &mut App) {
    let updates = cx.global::<AgentUpdates>();
    let keys = distinct_installation_keys(
        profiles
            .iter()
            .map(|profile| updates.register_profile(profile)),
    );
    let coordinator = updates.coordinator.clone();
    cx.spawn(async move |cx| {
        let worker = cx.background_executor().spawn(async move {
            for key in keys {
                let _ = coordinator.check(&key, true);
            }
        });
        worker.await;
        let _ = cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

pub(crate) fn schedule_startup_checks(cx: &mut App) {
    let updates = cx.global::<AgentUpdates>();
    if updates.testing() {
        return;
    }
    let coordinator = updates.coordinator.clone();
    let keys = coordinator
        .snapshots()
        .into_iter()
        .map(|snapshot| snapshot.identity.key)
        .collect::<Vec<_>>();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_secs(3)).await;
        let worker = cx.background_executor().spawn(async move {
            for key in keys {
                let _ = coordinator.check(&key, false);
            }
        });
        worker.await;
        let _ = cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpdateMode {
    WhenIdle,
    StopNow,
}

impl UpdateMode {
    fn interrupts_active_work(self) -> bool {
        self == Self::StopNow
    }
}

enum PreflightResolution {
    Ready(Vec<RecoverySnapshot>),
    Wait,
    Failed(String),
}

fn resolve_preflight(
    assessments: Vec<RecoveryReadiness>,
    mode: UpdateMode,
    stop_timeout_elapsed: bool,
) -> PreflightResolution {
    if let Some(message) = assessments.iter().find_map(|assessment| match assessment {
        RecoveryReadiness::MissingIdentity(message) => Some(message.clone()),
        _ => None,
    }) {
        return PreflightResolution::Failed(message);
    }
    if assessments
        .iter()
        .all(|assessment| matches!(assessment, RecoveryReadiness::Ready(_)))
    {
        return PreflightResolution::Ready(
            assessments
                .into_iter()
                .filter_map(|assessment| match assessment {
                    RecoveryReadiness::Ready(snapshot) => Some(snapshot),
                    _ => None,
                })
                .collect(),
        );
    }
    if mode.interrupts_active_work() && stop_timeout_elapsed {
        return PreflightResolution::Failed(
            "affected tabs did not reach a recoverable interruption boundary".to_string(),
        );
    }
    PreflightResolution::Wait
}

fn combine_transaction_error(
    operation_error: Option<UpdateError>,
    restore_failures: usize,
) -> Option<UpdateError> {
    if restore_failures == 0 {
        return operation_error;
    }
    Some(UpdateError::new(
        UpdateErrorKind::Recovery,
        operation_error.map_or_else(
            || format!("{restore_failures} agent tab(s) could not reconnect"),
            |error| format!("{error}; {restore_failures} agent tab(s) could not reconnect"),
        ),
    ))
}

pub(crate) fn request_update(key: InstallationKey, window: &mut Window, cx: &mut App) {
    let panes = matching_panes(&key, cx);
    let busy = panes
        .iter()
        .filter(|pane| {
            matches!(
                pane.read(cx).recovery_readiness(),
                RecoveryReadiness::Busy(_)
            )
        })
        .count();
    if busy == 0 {
        start_transaction(key, UpdateMode::WhenIdle, panes, cx);
        return;
    }

    window.open_dialog(cx, move |dialog, _, _| {
        let wait_key = key.clone();
        let stop_key = key.clone();
        dialog
            .title("Update agent provider")
            .overlay_closable(false)
            .content(move |content, _, cx| {
                content.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "{} affected tab(s) still have active work. Waiting is the safe default; stopping now interrupts current provider work.",
                            busy
                        )),
                )
            })
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("agent-update-cancel").label("Cancel")))
                    .child(
                        Button::new("agent-update-when-idle")
                            .primary()
                            .label("Update when idle")
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                let panes = matching_panes(&wait_key, cx);
                                start_transaction(
                                    wait_key.clone(),
                                    UpdateMode::WhenIdle,
                                    panes,
                                    cx,
                                );
                            }),
                    )
                    .child(
                        Button::new("agent-update-stop-now")
                            .danger()
                            .label("Stop now and update")
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                let panes = matching_panes(&stop_key, cx);
                                start_transaction(
                                    stop_key.clone(),
                                    UpdateMode::StopNow,
                                    panes,
                                    cx,
                                );
                            }),
                    ),
            )
    });
}

fn matching_panes(key: &InstallationKey, cx: &mut App) -> Vec<Entity<AgentPane>> {
    let shells = cx
        .global::<ShellRegistry>()
        .0
        .iter()
        .map(|entry| entry.shell.clone())
        .collect::<Vec<_>>();
    let mut panes = Vec::new();
    for shell in shells {
        let _ = shell.update(cx, |shell: &mut Shell, _| panes.extend(shell.agent_panes()));
    }
    let installations = panes
        .iter()
        .map(|pane| pane.read(cx).installation_key())
        .collect::<Vec<_>>();
    let affected = affected_installation_indices(key, &installations)
        .into_iter()
        .collect::<HashSet<_>>();
    panes
        .into_iter()
        .enumerate()
        .filter_map(|(index, pane)| affected.contains(&index).then_some(pane))
        .collect()
}

fn affected_installation_indices(
    target: &InstallationKey,
    installations: &[InstallationKey],
) -> Vec<usize> {
    installations
        .iter()
        .enumerate()
        .filter_map(|(index, key)| (key == target).then_some(index))
        .collect()
}

fn start_transaction(
    key: InstallationKey,
    mode: UpdateMode,
    panes: Vec<Entity<AgentPane>>,
    cx: &mut App,
) {
    let coordinator = cx.global::<AgentUpdates>().coordinator.clone();
    if coordinator.begin_update(&key).is_err() {
        cx.refresh_windows();
        return;
    }

    if mode.interrupts_active_work()
        && let Some(message) =
            panes
                .iter()
                .find_map(|pane| match pane.read(cx).recovery_identity_snapshot() {
                    RecoveryReadiness::MissingIdentity(message) => Some(message),
                    _ => None,
                })
    {
        coordinator.finish_update(
            &key,
            None,
            Some(UpdateError::new(UpdateErrorKind::Recovery, message)),
            0,
        );
        cx.refresh_windows();
        return;
    }

    for pane in &panes {
        pane.update(cx, |pane, cx| {
            if mode.interrupts_active_work() {
                pane.stop_active_work_for_update(cx);
            } else {
                pane.prepare_update_wait(cx);
            }
        });
    }
    cx.refresh_windows();

    cx.spawn(async move |cx| {
        let wait_started = Instant::now();
        let snapshots = loop {
            let assessments = cx.update(|cx| {
                panes
                    .iter()
                    .map(|pane| pane.read(cx).recovery_readiness())
                    .collect::<Vec<_>>()
            });

            match resolve_preflight(
                assessments,
                mode,
                wait_started.elapsed() >= Duration::from_secs(15),
            ) {
                PreflightResolution::Ready(snapshots) => break snapshots,
                PreflightResolution::Failed(message) => {
                    finish_preflight_failure(&coordinator, &key, &panes, message, cx);
                    return;
                }
                PreflightResolution::Wait => {}
            }
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
        };

        coordinator.transition(
            &key,
            UpdatePhase::Suspending,
            Some(UpdateProgress {
                completed: 0,
                total: panes.len(),
            }),
        );
        let suspension_tasks = cx.update(|cx| {
            panes
                .iter()
                .map(|pane| {
                    pane.update(cx, |pane, cx| {
                        pane.suspend_for_update(mode.interrupts_active_work(), cx)
                    })
                })
                .collect::<Vec<_>>()
        });
        let suspension_results = join_all(suspension_tasks).await;
        let suspended = suspension_results
            .iter()
            .enumerate()
            .filter_map(|(index, result)| result.is_ok().then_some(index))
            .collect::<Vec<_>>();

        if let Some(error) = suspension_results
            .iter()
            .find_map(|result| result.as_ref().err())
        {
            let error = UpdateError::new(UpdateErrorKind::Recovery, error);
            restore_tabs(&coordinator, &key, &panes, &snapshots, &suspended, cx).await;
            coordinator.finish_update(&key, None, Some(error), 0);
            let _ = cx.update(|cx| cx.refresh_windows());
            return;
        }

        coordinator.transition(&key, UpdatePhase::Updating, None);
        let _ = cx.update(|cx| {
            for pane in &panes {
                pane.update(cx, |pane, cx| pane.mark_provider_updating(cx));
            }
            cx.refresh_windows();
        });
        let update_coordinator = coordinator.clone();
        let update_key = key.clone();
        let update_result = cx
            .background_executor()
            .spawn(async move { update_coordinator.run_vendor_update(&update_key) })
            .await;

        let (verified, mut operation_error) = if let Err(error) = update_result {
            (None, Some(error))
        } else {
            coordinator.transition(&key, UpdatePhase::Verifying, None);
            let verify_coordinator = coordinator.clone();
            let verify_key = key.clone();
            match cx
                .background_executor()
                .spawn(async move { verify_coordinator.verify(&verify_key) })
                .await
            {
                Ok(status) => (Some(status), None),
                Err(error) => (None, Some(error)),
            }
        };

        let restore_failures =
            restore_tabs(&coordinator, &key, &panes, &snapshots, &suspended, cx).await;
        operation_error = combine_transaction_error(operation_error, restore_failures);
        coordinator.finish_update(&key, verified, operation_error, 0);
        let _ = cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

fn finish_preflight_failure(
    coordinator: &UpdateCoordinator,
    key: &InstallationKey,
    panes: &[Entity<AgentPane>],
    message: String,
    cx: &mut gpui::AsyncApp,
) {
    coordinator.finish_update(
        key,
        None,
        Some(UpdateError::new(UpdateErrorKind::Recovery, message)),
        0,
    );
    let _ = cx.update(|cx| {
        for pane in panes {
            pane.update(cx, |pane, cx| pane.cancel_update_wait(cx));
        }
        cx.refresh_windows();
    });
}

async fn restore_tabs(
    coordinator: &UpdateCoordinator,
    key: &InstallationKey,
    panes: &[Entity<AgentPane>],
    snapshots: &[RecoverySnapshot],
    suspended: &[usize],
    cx: &mut gpui::AsyncApp,
) -> usize {
    coordinator.transition(
        key,
        UpdatePhase::Restoring,
        Some(UpdateProgress {
            completed: 0,
            total: suspended.len(),
        }),
    );
    let mut failures = 0;
    for index in suspended.iter().copied() {
        let restored = cx.update(|cx| {
            panes[index].update(cx, |pane, cx| {
                pane.restore_after_update(&snapshots[index], cx)
            })
        });
        if !restored {
            failures += 1;
        }
    }

    let started = Instant::now();
    loop {
        let readiness = cx.update(|cx| {
            suspended
                .iter()
                .copied()
                .map(|index| panes[index].read(cx).restoration_readiness())
                .collect::<Vec<_>>()
        });
        let pending = readiness
            .iter()
            .filter(|state| matches!(state, RestorationReadiness::Pending))
            .count();
        let reported_failures = readiness
            .iter()
            .filter(|state| matches!(state, RestorationReadiness::Failed(_)))
            .count();
        failures = failures.max(reported_failures);
        coordinator.transition(
            key,
            UpdatePhase::Restoring,
            Some(UpdateProgress {
                completed: suspended.len() - pending,
                total: suspended.len(),
            }),
        );
        let _ = cx.update(|cx| cx.refresh_windows());
        if pending == 0 {
            break;
        }
        if started.elapsed() >= Duration::from_secs(30) {
            cx.update(|cx| {
                for (position, state) in readiness.iter().enumerate() {
                    if matches!(state, RestorationReadiness::Pending) {
                        let index = suspended[position];
                        panes[index].update(cx, |pane, cx| {
                            pane.fail_update_recovery(
                                "The provider did not become ready before the recovery timeout."
                                    .to_string(),
                                cx,
                            )
                        });
                    }
                }
            });
            failures += pending;
            coordinator.transition(
                key,
                UpdatePhase::Restoring,
                Some(UpdateProgress {
                    completed: suspended.len(),
                    total: suspended.len(),
                }),
            );
            break;
        }
        cx.background_executor()
            .timer(Duration::from_millis(100))
            .await;
    }
    failures
}

pub(crate) fn provider_for_profile(kind: AgentProfileKind) -> ProviderKind {
    match kind {
        AgentProfileKind::ClaudeCode => ProviderKind::Claude,
        AgentProfileKind::Codex => ProviderKind::Codex,
    }
}

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
        .unwrap_or_else(|| "unknown".to_string());
    let target = versions.and_then(|status| status.available.clone());
    let target_text = target
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| "unknown".to_string());
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
            format!("{provider} update available"),
            format!("{current} → {target_text}"),
            UpdateNotificationTone::Info,
            versions
                .is_some_and(|status| status.can_update)
                .then_some(NotificationPrimaryAction::Update),
            NotificationProgress::None,
            false,
        ),
        UpdatePhase::WaitingForIdle => (
            format!("Waiting to update {provider}"),
            "Agent tabs will be retained and updated when all affected work is recoverably idle."
                .to_string(),
            UpdateNotificationTone::Info,
            None,
            NotificationProgress::Indeterminate,
            false,
        ),
        UpdatePhase::Suspending => (
            format!("Stopping {provider} agents"),
            "Closing affected provider processes while keeping their tabs open.".to_string(),
            UpdateNotificationTone::Info,
            None,
            progress_view(snapshot),
            false,
        ),
        UpdatePhase::Updating => (
            format!("Updating {provider}"),
            format!("Installing {target_text} through the configured provider launcher."),
            UpdateNotificationTone::Info,
            None,
            NotificationProgress::Indeterminate,
            false,
        ),
        UpdatePhase::Verifying => (
            format!("Verifying {provider}"),
            "Checking the installed version after the provider updater finished.".to_string(),
            UpdateNotificationTone::Info,
            None,
            NotificationProgress::Indeterminate,
            false,
        ),
        UpdatePhase::Restoring => (
            format!("Restoring {provider} tabs"),
            "Reconnecting retained tabs to their provider conversations.".to_string(),
            UpdateNotificationTone::Info,
            None,
            progress_view(snapshot),
            false,
        ),
        UpdatePhase::Updated => (
            format!("{provider} updated"),
            format!("Installed version {current}. All affected tabs were restored."),
            UpdateNotificationTone::Success,
            None,
            NotificationProgress::Determinate(100.0),
            true,
        ),
        UpdatePhase::Unchanged => (
            format!("{provider} version unchanged"),
            bounded_error(
                snapshot,
                "The provider still reports an update as available.",
            ),
            UpdateNotificationTone::Warning,
            Some(NotificationPrimaryAction::Retry),
            NotificationProgress::Determinate(100.0),
            true,
        ),
        UpdatePhase::Failed => (
            format!("{provider} update failed"),
            bounded_error(snapshot, "The update could not be completed."),
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

struct UnavailableMaintenance {
    provider: ProviderKind,
    reason: String,
}

impl ProviderMaintenance for UnavailableMaintenance {
    fn provider(&self) -> ProviderKind {
        self.provider
    }

    fn probe(&self, _: &ConfiguredLauncher) -> Result<VersionStatus, UpdateError> {
        Err(UpdateError::new(UpdateErrorKind::Unsupported, &self.reason))
    }

    fn update(&self, _: &ConfiguredLauncher) -> Result<VendorUpdateResult, UpdateError> {
        Err(UpdateError::new(UpdateErrorKind::Unsupported, &self.reason))
    }
}

/// `--testing` exposes a complete fake workflow without touching provider
/// executables, release endpoints, or the production cache.
struct FakeMaintenance {
    provider: ProviderKind,
    updated: AtomicBool,
}

impl FakeMaintenance {
    fn new(provider: ProviderKind) -> Self {
        Self {
            provider,
            updated: AtomicBool::new(false),
        }
    }
}

impl ProviderMaintenance for FakeMaintenance {
    fn provider(&self) -> ProviderKind {
        self.provider
    }

    fn probe(&self, _: &ConfiguredLauncher) -> Result<VersionStatus, UpdateError> {
        let current = if self.updated.load(Ordering::SeqCst) {
            Version::new(1, 1, 0)
        } else {
            Version::new(1, 0, 0)
        };
        Ok(VersionStatus {
            provider: self.provider,
            current: Some(current),
            available: Some(Version::new(1, 1, 0)),
            install_method: Some("testing fixture".to_string()),
            channel: Some("testing".to_string()),
            can_update: true,
            support: DiscoverySupport::Supported,
            remediation: None,
        })
    }

    fn update(&self, _: &ConfiguredLauncher) -> Result<VendorUpdateResult, UpdateError> {
        self.updated.store(true, Ordering::SeqCst);
        Ok(VendorUpdateResult {
            diagnostic: "testing provider updated".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use nmt_agent_utils::update::{InstallationUpdateState, UpdateError, UpdateProgress};

    use super::*;

    fn snapshot(phase: UpdatePhase) -> InstallationSnapshot {
        let launcher = ConfiguredLauncher::new("fake-codex", []);
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
        second.identity = InstallationKey::derive(
            ProviderKind::Codex,
            &ConfiguredLauncher::new("another-codex", []),
        );
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
        let launcher = ConfiguredLauncher::new("this-executable-must-never-run", []);
        assert!(fake.probe(&launcher).unwrap().update_available());
        fake.update(&launcher).unwrap();
        assert!(!fake.probe(&launcher).unwrap().update_available());
    }

    #[test]
    fn mixed_installations_select_only_tabs_for_the_target_transaction() {
        let shared = InstallationKey::derive(
            ProviderKind::Claude,
            &ConfiguredLauncher::new("shared-claude", []),
        )
        .key;
        let unrelated = InstallationKey::derive(
            ProviderKind::Claude,
            &ConfiguredLauncher::new("other-claude", []),
        )
        .key;
        let installations = vec![shared.clone(), unrelated, shared.clone()];
        assert_eq!(
            affected_installation_indices(&shared, &installations),
            vec![0, 2]
        );
    }

    #[test]
    fn settings_installation_rows_deduplicate_profiles_in_first_seen_order() {
        let claude = InstallationKey::derive(
            ProviderKind::Claude,
            &ConfiguredLauncher::new("shared-claude", []),
        )
        .key;
        let codex = InstallationKey::derive(
            ProviderKind::Codex,
            &ConfiguredLauncher::new("shared-codex", []),
        )
        .key;

        assert_eq!(
            distinct_installation_keys([claude.clone(), claude.clone(), codex.clone(), claude,]),
            vec![
                InstallationKey::derive(
                    ProviderKind::Claude,
                    &ConfiguredLauncher::new("shared-claude", []),
                )
                .key,
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
        let installation = snapshot(UpdatePhase::Available).identity.key;
        let assessments = vec![
            RecoveryReadiness::Ready(RecoverySnapshot {
                installation: installation.clone(),
                identity: super::super::agent_pane::RecoveryIdentity::ClaudeSession(
                    "session-a".into(),
                ),
                profile_name: "Claude A".into(),
            }),
            RecoveryReadiness::Ready(RecoverySnapshot {
                installation,
                identity: super::super::agent_pane::RecoveryIdentity::ClaudeSession(
                    "session-b".into(),
                ),
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
}
