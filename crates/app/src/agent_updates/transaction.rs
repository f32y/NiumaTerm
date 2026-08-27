use std::collections::HashSet;
use std::time::{Duration, Instant};

use futures::future::join_all;
use gpui::prelude::*;
use gpui::{App, Entity, Window, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::{DIALOG_BUTTON_MIN_WIDTH, DialogClose, DialogFooter};
use gpui_component::{ActiveTheme as _, WindowExt as _};
use nmt_agent_utils::update::{
    InstallationKey, ProviderKind, UpdateCoordinator, UpdateError, UpdateErrorKind, UpdatePhase,
    UpdateProgress,
};
use nmt_app_agent::{AgentPane, RecoveryReadiness, RecoverySnapshot, RestorationReadiness};
use nmt_config::profile::AgentProfileKind;
use nmt_i18n::i18n;

use crate::agent_updates::AgentUpdates;
use crate::ui::Shell;
use crate::window::ShellRegistry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UpdateMode {
    WhenIdle,
    StopNow,
}

impl UpdateMode {
    pub(super) fn interrupts_active_work(self) -> bool {
        self == Self::StopNow
    }
}

pub(super) enum PreflightResolution {
    Ready(Vec<RecoverySnapshot>),
    Wait,
    Failed(String),
}

pub(super) fn resolve_preflight(
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
        return PreflightResolution::Failed(i18n("agent-update-interruption-timeout").to_string());
    }
    PreflightResolution::Wait
}

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
    let panes = matching_panes(&key, cx);
    let busy = panes
        .iter()
        .filter(|pane| {
            matches!(
                pane.read(cx).recovery_readiness(cx),
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
                            .min_w(DIALOG_BUTTON_MIN_WIDTH)
                            .danger()
                            .label(i18n("agent-update-dialog-stop-now"))
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                let panes = matching_panes(&stop_key, cx);
                                start_transaction(stop_key.clone(), UpdateMode::StopNow, panes, cx);
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
                .find_map(|pane| match pane.read(cx).recovery_identity_snapshot(cx) {
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
                    .map(|pane| pane.read(cx).recovery_readiness(cx))
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
            cx.update(|cx| cx.refresh_windows());
            return;
        }

        coordinator.transition(&key, UpdatePhase::Updating, None);
        cx.update(|cx| {
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
        cx.update(|cx| cx.refresh_windows());
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
    cx.update(|cx| {
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
    // A restart failure now surfaces through `restoration_readiness` below,
    // because the backend process comes up off the UI thread.
    let mut failures = 0;
    for index in suspended.iter().copied() {
        cx.update(|cx| {
            panes[index].update(cx, |pane, cx| {
                pane.restore_after_update(&snapshots[index], cx)
            })
        });
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
        cx.update(|cx| cx.refresh_windows());
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
                                i18n("agent-update-recovery-timeout").to_string(),
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
