use std::{collections, time};

use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, SharedString, px};
use gpui_component::notification::Notification;
use gpui_component::{Icon, IconNamed, v_flex};
use nmt_agent::update::{ProviderKind, UpdatePhase};
use rust_i18n::t;

use crate::agent_updates;
use crate::agent_updates::{
    AgentUpdates, FocusedVisibleLifetime, NotificationPrimaryAction, UpdateNotificationView,
};
use crate::ui::notification_card::{NotificationAction, NotificationCard};
use crate::ui::shell::AppWindow;
use crate::ui::shell::actions::ShowSettings;
#[cfg(windows)]
use crate::update::UpdateNotification;

/// One on-screen provider-update notification: the reduced view it was built
/// from, the card entity presenting it, and, for the auto-hiding phases, the
/// focused-visible clock that retires it. One record per key, so the three
/// cannot fall out of step.
pub(super) struct UpdateCard {
    view: UpdateNotificationView,
    card: Entity<Notification>,
    elapsed: Option<FocusedVisibleLifetime>,
}

/// Provider and application updates share one stack so their cards cannot
/// overlap. Provider cards have one entry per coordinator key and one polling
/// task for auto-hiding phases; the task flag prevents duplicate timers.
#[derive(Default)]
pub(super) struct UpdateNotificationLayer {
    cards: collections::HashMap<String, UpdateCard>,
    timer_running: bool,
    #[cfg(windows)]
    app: UpdateNotification,
}

fn update_notification_card(
    view: UpdateNotificationView,
    shell: gpui::WeakEntity<AppWindow>,
) -> Notification {
    let icon = match view.provider {
        ProviderKind::Claude => Icon::new(ClaudeUpdateIcon),
        ProviderKind::Codex => Icon::new(CodexUpdateIcon),
    };

    let close_key = view.installation.clone();
    let close_target = view.target.clone();
    let close_phase = view.phase;

    let primary = view.primary.map(|action| {
        let label = match action {
            NotificationPrimaryAction::Update => t!("shell-updates-update"),
            NotificationPrimaryAction::Retry => t!("shell-updates-retry"),
        };

        NotificationAction::new(label, move |window, cx| {
            agent_updates::request_update(view.installation.clone(), window, cx);
        })
    });

    NotificationCard {
        title: view.title.into(),
        message: view.message.into(),
        tone: view.tone,
        icon: Some(icon),
        progress: view.progress,
        primary,
        secondary: Some(NotificationAction::new(
            t!("shell-updates-settings"),
            move |window, cx| {
                let _ = shell.update(cx, |shell, cx| {
                    shell.on_show_settings(&ShowSettings, window, cx)
                });
            },
        )),
    }
    .build()
    .id1::<AgentUpdateNotification>(view.key)
    .on_close(move |_, cx| {
        let Some(updates) = cx.try_global::<AgentUpdates>() else {
            return;
        };

        if close_phase == UpdatePhase::Available {
            if let Some(target) = close_target.as_ref() {
                updates.coordinator.dismiss_available(&close_key, target);
            }
        } else {
            updates.coordinator.hide_notification(&close_key);
        }

        AgentUpdates::notify_changed(cx);
    })
}

impl UpdateNotificationLayer {
    pub(super) fn render(&mut self, cx: &mut Context<AppWindow>) -> Option<AnyElement> {
        let snapshots = cx.global::<AgentUpdates>().coordinator.snapshots();

        let views = snapshots
            .iter()
            .filter_map(agent_updates::notification_view)
            .collect::<Vec<_>>();

        let active_keys = views
            .iter()
            .map(|view| view.key.clone())
            .collect::<collections::HashSet<_>>();

        self.cards.retain(|key, _| active_keys.contains(key));

        let shell = cx.weak_entity();

        let mut cards = Vec::with_capacity(views.len());

        for view in views {
            let entry = match self.cards.entry(view.key.clone()) {
                collections::hash_map::Entry::Occupied(occupied) => {
                    let entry = occupied.into_mut();

                    if entry.view != view {
                        entry.card.update(cx, |card, _| {
                            *card = update_notification_card(view.clone(), shell.clone())
                        });
                    }

                    entry.view = view;

                    entry
                }
                collections::hash_map::Entry::Vacant(vacant) => {
                    let card = cx.new(|_| update_notification_card(view.clone(), shell.clone()));

                    vacant.insert(UpdateCard {
                        view,
                        card,
                        elapsed: None,
                    })
                }
            };

            // Auto-hiding phases keep a focused-visible clock; entering a
            // sticky one clears it, so a card cannot expire on time banked
            // while it still counted down.
            if entry.view.terminal_timeout {
                let phase = entry.view.phase;

                entry
                    .elapsed
                    .get_or_insert_with(|| FocusedVisibleLifetime::new(phase))
                    .set_phase(phase);
            } else {
                entry.elapsed = None;
            }

            cards.push(entry.card.clone());
        }

        self.ensure_timer(cx);

        #[cfg(windows)]
        cards.extend(self.app.render(cx));

        (!cards.is_empty()).then(|| {
            v_flex()
                .absolute()
                .top(px(52.))
                .right(px(16.))
                .w_112()
                .gap_2()
                .children(cards)
                .into_any_element()
        })
    }

    fn ensure_timer(&mut self, cx: &mut Context<AppWindow>) {
        let any_expiring = self.cards.values().any(|entry| entry.elapsed.is_some());

        if self.timer_running || !any_expiring {
            return;
        }

        self.timer_running = true;

        cx.spawn(async move |shell, cx| {
            loop {
                cx.background_executor()
                    .timer(time::Duration::from_millis(100))
                    .await;

                let keep_running = shell.update(cx, expire_elapsed_cards).unwrap_or(false);

                if !keep_running {
                    let _ = shell.update(cx, |shell, _| {
                        shell.update_notifications.timer_running = false;
                    });

                    break;
                }
            }
        })
        .detach();
    }
}

fn expire_elapsed_cards(shell: &mut AppWindow, cx: &mut Context<AppWindow>) -> bool {
    let mut expired = Vec::new();

    if shell.window_active {
        for (key, entry) in &mut shell.update_notifications.cards {
            if let Some(lifetime) = &mut entry.elapsed
                && lifetime.tick(true, time::Duration::from_millis(100))
            {
                expired.push(key.clone());
            }
        }
    }

    if !expired.is_empty() {
        let coordinator = cx.global::<AgentUpdates>().coordinator.clone();

        for key in &expired {
            if let Some(entry) = shell.update_notifications.cards.get_mut(key) {
                coordinator.hide_notification(&entry.view.installation);

                // The card stays until the coordinator's
                // next snapshot retires its key; only the
                // clock is spent.
                entry.elapsed = None;
            }
        }

        AgentUpdates::notify_changed(cx);
    }

    shell
        .update_notifications
        .cards
        .values()
        .any(|entry| entry.elapsed.is_some())
}

struct ClaudeUpdateIcon;

impl IconNamed for ClaudeUpdateIcon {
    fn path(self) -> SharedString {
        "icons/claude.svg".into()
    }
}

struct CodexUpdateIcon;

impl IconNamed for CodexUpdateIcon {
    fn path(self) -> SharedString {
        "icons/codex.svg".into()
    }
}

struct AgentUpdateNotification;
