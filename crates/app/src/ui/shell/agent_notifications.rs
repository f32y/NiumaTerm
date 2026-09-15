use crate::ui::shell::*;

pub(super) struct AgentNotificationState {
    pub(super) agent_monitor: AgentMonitor,

    agent_timer_generation: u64,

    /// A conversation from another directory, waiting for a render to open it
    /// in a tab rooted there. Event subscriptions carry no window, and opening
    /// a tab needs one.
    pub(super) pending_agent_resume: Option<PendingAgentResume>,

    /// A tab whose agent asked to be closed, waiting for a render to close it.
    /// Closing a tab needs a window for the same reason opening one does.
    pub(super) pending_agent_close: Option<TabId>,
}

impl AgentNotificationState {
    pub(super) fn new(agent_monitor: AgentMonitor) -> Self {
        Self {
            agent_monitor,
            agent_timer_generation: 0,
            pending_agent_resume: None,
            pending_agent_close: None,
        }
    }

    pub(super) fn reschedule_agent_timer(&mut self, cx: &mut Context<Shell>) {
        self.agent_timer_generation = self.agent_timer_generation.wrapping_add(1);

        let generation = self.agent_timer_generation;

        let Some(deadline) = self.agent_monitor.next_deadline() else {
            return;
        };

        let delay = deadline.saturating_duration_since(time::Instant::now());

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;

            let _ = this.update(cx, |this, cx| {
                this.process_due_agent_deadlines(generation, cx)
            });
        })
        .detach();
    }

    /// Stop tracking `route`, whose pane or tab is gone.
    pub(super) fn remove_route(&mut self, route: &AgentRoute, cx: &mut Context<Shell>) {
        let mutation = self.agent_monitor.remove_route(route);

        apply_monitor_display_change(&mutation, cx);

        self.reschedule_agent_timer(cx);
    }

    /// Mark notification `notification_id` of `route` read. Returns whether
    /// that changed anything on screen.
    pub(super) fn acknowledge(
        &mut self,
        route: &AgentRoute,
        notification_id: &str,
        cx: &mut Context<Shell>,
    ) -> bool {
        let mutation = self.agent_monitor.acknowledge(route, notification_id);

        apply_monitor_display_change(&mutation, cx);

        mutation.visible_changed
    }

    /// Offer each pending notification to the system once. The one for
    /// `visible_route`, the agent on screen in an active window, is read on
    /// arrival instead.
    pub(super) fn process_native_notifications(
        &mut self,
        visible_route: Option<&AgentRoute>,
        cx: &mut Context<Shell>,
    ) {
        let system_notifications_enabled = cx
            .global::<AppSettings>()
            .config()
            .system
            .send_system_notifications
            && system_notification_enabled();

        for notification in self.agent_monitor.pending_native_notifications() {
            if !request_native_delivery(visible_route, &notification.route) {
                self.acknowledge(&notification.route, &notification.id, cx);

                continue;
            }

            if !self
                .agent_monitor
                .mark_native_requested(&notification.route, &notification.id)
            {
                continue;
            }

            if !system_notifications_enabled {
                continue;
            }

            let activation_url: String = (&CliAction::FocusNotification {
                route: notification.route.clone(),
                notification_id: notification.id.clone(),
            })
                .into();

            thread::spawn(move || {
                match show_notification(&NativeNotification {
                    title: notification.title,
                    body: notification.body,
                    activation_url,
                    tag: notification.native_tag,
                    group: notification.native_group,
                }) {
                    Ok(()) => {}
                    Err(error) => warn!("native notification failed: {error}"),
                }
            });
        }
    }

    pub(super) fn process_due(&mut self, generation: u64) -> Option<MonitorMutation> {
        if self.agent_timer_generation != generation {
            return None;
        }

        Some(self.agent_monitor.process_due(time::Instant::now()))
    }
}

/// Carry a monitor change to the screen: withdraw the system notifications
/// it removed, and repaint when what the chrome shows changed.
pub(super) fn apply_monitor_display_change(mutation: &MonitorMutation, cx: &mut Context<Shell>) {
    remove_native_notifications(&mutation.removed_notifications);

    if mutation.visible_changed {
        cx.notify();
    }
}

/// Withdraw `notifications` from the system tray, off the UI thread.
pub(super) fn remove_native_notifications(notifications: &[AgentNotification]) {
    for notification in notifications {
        let tag = notification.native_tag.clone();
        let group = notification.native_group.clone();

        thread::spawn(move || {
            let _ = remove_notification(&tag, &group);
        });
    }
}
