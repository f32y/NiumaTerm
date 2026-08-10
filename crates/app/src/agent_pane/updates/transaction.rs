use std::collections::HashSet;
use std::time::{Duration, Instant};

use futures::future::join_all;
use gpui::prelude::*;
use gpui::{App, Entity, Window, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::{DialogClose, DialogFooter};
use gpui_component::{ActiveTheme as _, WindowExt as _};
use nmt_agent_utils::update::{
    InstallationKey, ProviderKind, UpdateCoordinator, UpdateError, UpdateErrorKind, UpdatePhase,
    UpdateProgress,
};
use nmt_config::profile::AgentProfileKind;

use crate::agent_pane::updates::AgentUpdates;
use crate::agent_pane::{AgentPane, RecoveryReadiness, RecoverySnapshot, RestorationReadiness};
use crate::ui::Shell;
use crate::window::ShellRegistry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::agent_pane::updates) enum UpdateMode {
    WhenIdle,
    StopNow,
}

impl UpdateMode {
    pub(in crate::agent_pane::updates) fn interrupts_active_work(self) -> bool {
        self == Self::StopNow
    }
}

pub(in crate::agent_pane::updates) enum PreflightResolution {
    Ready(Vec<RecoverySnapshot>),
    Wait,
    Failed(String),
}

pub(in crate::agent_pane::updates) fn resolve_preflight(
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

pub(in crate::agent_pane::updates) fn combine_transaction_error(
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

pub(in crate::agent_pane::updates) fn affected_installation_indices(
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
