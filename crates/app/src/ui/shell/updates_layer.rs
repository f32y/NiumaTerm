use nmt_i18n::i18n;

use crate::ui::shell::*;

/// One on-screen provider-update notification: the reduced view it was built
/// from, the card entity presenting it, and, for the auto-hiding phases, the
/// focused-visible clock that retires it. One record per key, so the three
/// cannot fall out of step.
pub(super) struct UpdateCard {
    view: UpdateNotificationView,
    card: Entity<Notification>,
    elapsed: Option<FocusedVisibleLifetime>,
}

/// The on-screen provider-update notifications: one card per coordinator key
/// plus the single polling task that retires the auto-hiding ones. The task
/// flag belongs beside the cards because it may only run while some card still
/// has a clock, and it is what stops a second task being spawned.
#[derive(Default)]
pub(super) struct UpdateNotificationLayer {
    cards: collections::HashMap<String, UpdateCard>,
    timer_running: bool,
}

fn update_notification_card(
    view: UpdateNotificationView,
    shell: gpui::WeakEntity<Shell>,
) -> Notification {
    let tone = match view.tone {
        UpdateNotificationTone::Info => NotificationType::Info,
        UpdateNotificationTone::Success => NotificationType::Success,
        UpdateNotificationTone::Warning => NotificationType::Warning,
        UpdateNotificationTone::Error => NotificationType::Error,
    };
    let icon = match view.provider {
        ProviderKind::Claude => Icon::new(ClaudeUpdateIcon),
        ProviderKind::Codex => Icon::new(CodexUpdateIcon),
    };
    let close_key = view.installation.clone();
    let close_target = view.target.clone();
    let close_phase = view.phase;
    let progress = view.progress.clone();
    let progress_key = view.key.clone();
    let settings_key = view.key.clone();
    let settings_shell = shell.clone();

    let mut notification = Notification::new()
        .id1::<AgentUpdateNotification>(view.key.clone())
        .placement(Anchor::TopRight)
        .with_type(tone)
        .icon(icon)
        .title(view.title.clone())
        .message(view.message.clone())
        .autohide(false)
        .content(move |_, _, _| {
            let progress_bar = match progress {
                NotificationProgress::None => None,
                NotificationProgress::Indeterminate => Some(
                    Progress::new(format!("{progress_key}-progress"))
                        .loading(true)
                        .into_any_element(),
                ),
                NotificationProgress::Determinate(value) => Some(
                    Progress::new(format!("{progress_key}-progress"))
                        .value(value)
                        .into_any_element(),
                ),
            };
            let has_progress = progress_bar.is_some();
            v_flex()
                .w_full()
                .when(has_progress, |this| this.pt_2())
                .children(progress_bar)
                .into_any_element()
        })
        .secondary_action(move |_, _, _| {
            let settings_shell = settings_shell.clone();
            Button::new(format!("{settings_key}-settings"))
                .ghost()
                .label(i18n("shell-updates-settings"))
                .on_click(move |_, window, cx| {
                    let _ = settings_shell.update(cx, |shell, cx| {
                        shell.on_show_settings(&ShowSettings, window, cx)
                    });
                })
        })
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
            cx.refresh_windows();
        });

    if let Some(primary) = view.primary {
        let action_key = view.installation.clone();
        notification = notification.action(move |_, _, _| {
            Button::new(format!("{}-primary", action_key.as_str()))
                .primary()
                .label(match primary {
                    NotificationPrimaryAction::Update => i18n("shell-updates-update"),
                    NotificationPrimaryAction::Retry => i18n("shell-updates-retry"),
                })
                .on_click({
                    let action_key = action_key.clone();
                    move |_, window, cx| {
                        agent_updates::request_update(action_key.clone(), window, cx)
                    }
                })
        });
    }
    notification
}

impl UpdateNotificationLayer {
    pub(super) fn render(&mut self, cx: &mut Context<Shell>) -> Option<AnyElement> {
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

    fn ensure_timer(&mut self, cx: &mut Context<Shell>) {
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
                let keep_running = shell
                    .update(cx, |shell, cx| {
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
                            cx.refresh_windows();
                        }
                        shell
                            .update_notifications
                            .cards
                            .values()
                            .any(|entry| entry.elapsed.is_some())
                    })
                    .unwrap_or(false);
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
