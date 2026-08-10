use crate::ui::shell::*;

impl Shell {
    fn update_notification_card(
        view: UpdateNotificationView,
        shell: gpui::WeakEntity<Self>,
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
                    .label("Settings")
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
                        NotificationPrimaryAction::Update => "Update",
                        NotificationPrimaryAction::Retry => "Retry",
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

    pub(super) fn render_update_notification_layer(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let snapshots = cx.global::<AgentUpdates>().coordinator.snapshots();
        let views = snapshots
            .iter()
            .filter_map(agent_updates::notification_view)
            .collect::<Vec<_>>();
        let active_keys = views
            .iter()
            .map(|view| view.key.clone())
            .collect::<collections::HashSet<_>>();
        self.update_notifications
            .retain(|key, _| active_keys.contains(key));
        self.update_notification_views
            .retain(|key, _| active_keys.contains(key));

        let terminal_keys = views
            .iter()
            .filter(|view| view.terminal_timeout)
            .map(|view| (view.key.clone(), view.phase))
            .collect::<collections::HashMap<_, _>>();
        self.update_terminal_elapsed
            .retain(|key, _| terminal_keys.contains_key(key));
        for (key, phase) in terminal_keys {
            let timer = self
                .update_terminal_elapsed
                .entry(key)
                .or_insert_with(|| FocusedVisibleLifetime::new(phase));
            timer.set_phase(phase);
        }

        let shell = cx.weak_entity();
        let mut cards = Vec::with_capacity(views.len());
        for view in views {
            let changed = self
                .update_notification_views
                .get(&view.key)
                .is_none_or(|previous| previous != &view);
            let card = if let Some(card) = self.update_notifications.get(&view.key) {
                if changed {
                    card.update(cx, |card, _| {
                        *card = Self::update_notification_card(view.clone(), shell.clone())
                    });
                }
                card.clone()
            } else {
                let card = cx.new(|_| Self::update_notification_card(view.clone(), shell.clone()));
                self.update_notifications
                    .insert(view.key.clone(), card.clone());
                card
            };
            self.update_notification_views
                .insert(view.key.clone(), view);
            cards.push(card);
        }

        self.ensure_update_notification_timer(cx);
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

    fn ensure_update_notification_timer(&mut self, cx: &mut Context<Self>) {
        if self.update_notification_timer_running || self.update_terminal_elapsed.is_empty() {
            return;
        }
        self.update_notification_timer_running = true;
        cx.spawn(async move |shell, cx| {
            loop {
                cx.background_executor()
                    .timer(time::Duration::from_millis(100))
                    .await;
                let keep_running = shell
                    .update(cx, |shell, cx| {
                        let mut expired = Vec::new();
                        if shell.window_active {
                            for (key, lifetime) in &mut shell.update_terminal_elapsed {
                                if lifetime.tick(true, time::Duration::from_millis(100)) {
                                    expired.push(key.clone());
                                }
                            }
                        }
                        if !expired.is_empty() {
                            let coordinator = cx.global::<AgentUpdates>().coordinator.clone();
                            for key in &expired {
                                if let Some(view) = shell.update_notification_views.get(key) {
                                    coordinator.hide_notification(&view.installation);
                                }
                                shell.update_terminal_elapsed.remove(key);
                            }
                            cx.refresh_windows();
                        }
                        !shell.update_terminal_elapsed.is_empty()
                    })
                    .unwrap_or(false);
                if !keep_running {
                    let _ = shell.update(cx, |shell, _| {
                        shell.update_notification_timer_running = false;
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
