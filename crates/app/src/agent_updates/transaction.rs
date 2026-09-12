use std::collections::HashSet;
use std::time::{Duration, Instant};

use futures::future::join_all;
use gpui::prelude::*;
use gpui::{App, AsyncApp, Entity, Window, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::{DIALOG_BUTTON_MIN_WIDTH, DialogClose, DialogFooter};
use gpui_component::{ActiveTheme as _, WindowExt as _};
use nmt_agent::session::lifecycle::{RecoveryReadiness, RecoverySnapshot, RestorationReadiness};
use nmt_agent::update::{
    InstallationKey, ProviderKind, UpdateCoordinator, UpdateError, UpdateErrorKind, UpdatePhase,
    UpdateProgress, VersionStatus,
};
use nmt_agent_ui::execution::{AgentSession, SessionRegistry};
use nmt_config::profile::AgentProfileKind;
use nmt_i18n::i18n;

use crate::agent_updates::AgentUpdates;
use crate::agent_updates::maintenance::{
    PreflightFailure, UpdateEnvironment, UpdateMode, run_transaction,
};

pub(super) fn combine_transaction_error(
    operation_error: Option<UpdateError>,
    restore_failures: usize,
) -> Option<UpdateError> {
    if restore_failures == 0 {
        return operation_error;
    }

    Some(UpdateError::new(
        UpdateErrorKind::Recovery,
        operation_error.map_or_else(
            || {
                i18n("agent-update-reconnect-failures")
                    .replace("{count}", &restore_failures.to_string())
            },
            |error| {
                i18n("agent-update-error-with-reconnect-failures")
                    .replace("{error}", error.message())
                    .replace("{count}", &restore_failures.to_string())
            },
        ),
    ))
}

pub(crate) fn request_update(key: InstallationKey, window: &mut Window, cx: &mut App) {
    let sessions = matching_sessions(&key, cx);

    let busy = sessions
        .iter()
        .filter(|session| {
            matches!(
                session.read(cx).recovery_readiness(cx),
                RecoveryReadiness::Busy(_)
            )
        })
        .count();

    if busy == 0 {
        start_transaction(key, UpdateMode::WhenIdle, sessions, cx);

        return;
    }

    window.open_dialog(cx, move |dialog, _, _| {
        let wait_key = key.clone();
        let stop_key = key.clone();

        dialog
            .title(i18n("agent-update-dialog-title"))
            .overlay_closable(false)
            .content(move |content, _, cx| {
                content.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            i18n("agent-update-dialog-active-work")
                                .replace("{count}", &busy.to_string()),
                        ),
                )
            })
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("agent-update-when-idle")
                            .min_w(DIALOG_BUTTON_MIN_WIDTH)
                            .primary()
                            .label(i18n("agent-update-dialog-when-idle"))
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);

                                let sessions = matching_sessions(&wait_key, cx);

                                start_transaction(
                                    wait_key.clone(),
                                    UpdateMode::WhenIdle,
                                    sessions,
                                    cx,
                                );
                            }),
                    )
                    .child(
                        Button::new("agent-update-stop-now")
                            .min_w(DIALOG_BUTTON_MIN_WIDTH)
                            .danger()
                            .label(i18n("agent-update-dialog-stop-now"))
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);

                                let sessions = matching_sessions(&stop_key, cx);

                                start_transaction(
                                    stop_key.clone(),
                                    UpdateMode::StopNow,
                                    sessions,
                                    cx,
                                );
                            }),
                    )
                    .child(
                        DialogClose::new().child(
                            Button::new("agent-update-cancel")
                                .min_w(DIALOG_BUTTON_MIN_WIDTH)
                                .label(i18n("agent-update-dialog-cancel")),
                        ),
                    ),
            )
    });
}

fn matching_sessions(key: &InstallationKey, cx: &mut App) -> Vec<Entity<AgentSession>> {
    let sessions = SessionRegistry::sessions(cx);

    let installations = sessions
        .iter()
        .map(|session| session.read(cx).installation_key())
        .collect::<Vec<_>>();

    let affected = affected_installation_indices(key, &installations)
        .into_iter()
        .collect::<HashSet<_>>();

    sessions
        .into_iter()
        .enumerate()
        .filter_map(|(index, session)| affected.contains(&index).then_some(session))
        .collect()
}

/// A tab whose harness has no vendor-managed installation carries no key, so it
/// matches no target and is never suspended by another harness's update.
pub(super) fn affected_installation_indices(
    target: &InstallationKey,
    installations: &[Option<InstallationKey>],
) -> Vec<usize> {
    installations
        .iter()
        .enumerate()
        .filter_map(|(index, key)| (key.as_ref() == Some(target)).then_some(index))
        .collect()
}

fn start_transaction(
    key: InstallationKey,
    mode: UpdateMode,
    sessions: Vec<Entity<AgentSession>>,
    cx: &mut App,
) {
    let coordinator = cx.global::<AgentUpdates>().coordinator.clone();

    if coordinator.begin_update(&key).is_err() {
        cx.refresh_windows();

        return;
    }

    cx.refresh_windows();

    cx.spawn(async move |cx| {
        let mut environment = SessionUpdateEnvironment {
            sessions,
            coordinator: coordinator.clone(),
            key: key.clone(),
            started: Instant::now(),
            cx,
        };

        let (verified, error) = match run_transaction(&mut environment, mode).await {
            Ok(outcome) => (
                outcome.verified,
                combine_transaction_error(outcome.operation_error, outcome.restore_failures),
            ),

            Err(error) => {
                let message = match error {
                    PreflightFailure::MissingIdentity(message) => message,

                    PreflightFailure::InterruptionTimeout => {
                        i18n("agent-update-interruption-timeout").to_string()
                    }
                };

                (
                    None,
                    Some(UpdateError::new(UpdateErrorKind::Recovery, message)),
                )
            }
        };

        coordinator.finish_update(&key, verified, error, 0);
        environment.cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

struct SessionUpdateEnvironment<'a> {
    sessions: Vec<Entity<AgentSession>>,
    coordinator: UpdateCoordinator,
    key: InstallationKey,
    started: Instant,
    cx: &'a mut AsyncApp,
}

impl UpdateEnvironment for SessionUpdateEnvironment<'_> {
    fn identity_failure(&mut self) -> Option<String> {
        self.cx.update(|cx| {
            self.sessions.iter().find_map(|session| {
                match session.read(cx).recovery_identity_snapshot(cx) {
                    RecoveryReadiness::MissingIdentity(message) => Some(message),
                    _ => None,
                }
            })
        })
    }

    fn prepare(&mut self, mode: UpdateMode) {
        self.cx.update(|cx| {
            for session in &self.sessions {
                session.update(cx, |session, cx| match mode {
                    UpdateMode::StopNow => session.stop_active_work_for_update(cx),
                    UpdateMode::WhenIdle => session.prepare_update_wait(cx),
                });
            }

            cx.refresh_windows();
        });
    }

    fn readiness(&mut self) -> Vec<RecoveryReadiness> {
        self.cx.update(|cx| {
            self.sessions
                .iter()
                .map(|session| session.read(cx).recovery_readiness(cx))
                .collect()
        })
    }

    fn cancel_wait(&mut self) {
        self.cx.update(|cx| {
            for session in &self.sessions {
                session.update(cx, |session, cx| session.cancel_update_wait(cx));
            }

            cx.refresh_windows();
        });
    }

    async fn suspend(&mut self, mode: UpdateMode) -> Vec<Result<(), String>> {
        let tasks = self.cx.update(|cx| {
            self.sessions
                .iter()
                .map(|session| {
                    session.update(cx, |session, cx| {
                        session.suspend_for_update(mode.interrupts_active_work(), cx)
                    })
                })
                .collect::<Vec<_>>()
        });

        join_all(tasks).await
    }

    async fn update(&mut self) -> Result<(), UpdateError> {
        self.cx.update(|cx| {
            for session in &self.sessions {
                session.update(cx, |session, cx| session.mark_provider_updating(cx));
            }

            cx.refresh_windows();
        });

        let coordinator = self.coordinator.clone();
        let key = self.key.clone();

        self.cx
            .background_executor()
            .spawn(async move { coordinator.run_vendor_update(&key).map(|_| ()) })
            .await
    }

    async fn verify(&mut self) -> Result<VersionStatus, UpdateError> {
        let coordinator = self.coordinator.clone();
        let key = self.key.clone();

        self.cx
            .background_executor()
            .spawn(async move { coordinator.verify(&key) })
            .await
    }

    fn restore(&mut self, snapshots: &[RecoverySnapshot], suspended: &[usize]) {
        self.cx.update(|cx| {
            for &index in suspended {
                self.sessions[index].update(cx, |session, cx| {
                    session.restore_after_update(&snapshots[index], cx)
                });
            }
        });
    }

    fn restoration_readiness(&mut self, suspended: &[usize]) -> Vec<RestorationReadiness> {
        self.cx.update(|cx| {
            suspended
                .iter()
                .map(|&index| self.sessions[index].read(cx).restoration_readiness())
                .collect()
        })
    }

    fn recovery_timed_out(&mut self, pending: &[usize]) {
        self.cx.update(|cx| {
            for &index in pending {
                self.sessions[index].update(cx, |session, cx| {
                    session
                        .fail_update_recovery(i18n("agent-update-recovery-timeout").to_string(), cx)
                });
            }
        });
    }

    fn publish(&mut self, phase: UpdatePhase, progress: Option<UpdateProgress>) {
        self.coordinator.transition(&self.key, phase, progress);
        self.cx.update(|cx| cx.refresh_windows());
    }

    fn now(&self) -> Duration {
        self.started.elapsed()
    }

    async fn wait(&mut self, duration: Duration) {
        self.cx.background_executor().timer(duration).await;
    }
}

/// The updatable installation this profile resolves to. `None` means the
/// harness is installed and updated through the user's own package manager, so
/// there is nothing for the update surface to probe or replace.
pub(crate) fn provider_for_profile(kind: AgentProfileKind) -> Option<ProviderKind> {
    match kind {
        AgentProfileKind::ClaudeCode => Some(ProviderKind::Claude),
        AgentProfileKind::Codex => Some(ProviderKind::Codex),
        AgentProfileKind::DeepSeek => None,
    }
}
