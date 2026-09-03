use nmt_platform::windows::window::is_foreground_and_not_minimized;

use crate::ui::shell::*;

struct AgentRouteLocation {
    workspace_id: WorkspaceId,
    workspace_index: usize,
    tab_id: TabId,
    tab_index: usize,
    target: AgentRouteTarget,
}

enum AgentRouteTarget {
    Terminal {
        pane_id: PaneId,
        pane: Entity<TerminalPane>,
    },
    Agent(Entity<AgentPane>),
}

impl Shell {
    pub(super) fn register_agent_pane(&mut self, pane: &Entity<TerminalPane>, cx: &App) {
        self.agent_monitor.register_route(
            pane.read(cx).agent_route().clone(),
            AgentActivityPolicy::ExpireAfterInactivity,
            time::Instant::now(),
        );
    }

    pub(super) fn register_agent_tab(&mut self, pane: &Entity<AgentPane>, cx: &App) {
        self.agent_monitor.register_route(
            pane.read(cx).agent_route().clone(),
            AgentActivityPolicy::ExplicitLifecycle,
            time::Instant::now(),
        );
    }

    pub(super) fn remove_agent_route(&mut self, route: &AgentRoute, cx: &mut Context<Self>) {
        let mutation = self.agent_monitor.remove_route(route);

        Self::remove_native_notifications(&mutation.removed_notifications);

        if mutation.visible_changed {
            cx.notify();
        }

        self.reschedule_agent_timer(cx);
    }

    pub(super) fn remove_native_notifications(notifications: &[AgentNotification]) {
        for notification in notifications {
            let tag = notification.native_tag.clone();
            let group = notification.native_group.clone();

            thread::spawn(move || {
                let _ = remove_notification(&tag, &group);
            });
        }
    }

    pub(super) fn exact_window_active(window: &Window) -> bool {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return false;
        };

        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return false;
        };

        // GetForegroundWindow answers this on its own: only the foreground
        // top-level window holds keyboard focus. GPUI's cached activation bit
        // is skipped here because it starts out false and is only refreshed
        // from WM_ACTIVATE, which the window misses when it is shown already
        // activated -- leaving the bit false until the user clicks or
        // alt-tabs, long after the window is genuinely in front.
        is_foreground_and_not_minimized(handle.hwnd)
    }

    pub(super) fn acknowledge_notification(
        &mut self,
        route: &AgentRoute,
        notification_id: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let mutation = self.agent_monitor.acknowledge(route, notification_id);

        Self::remove_native_notifications(&mutation.removed_notifications);

        if mutation.visible_changed {
            cx.notify();
        }

        mutation.visible_changed
    }

    pub(super) fn process_native_notifications(&mut self, cx: &mut Context<Self>) {
        let system_notifications_enabled = system_notification_enabled();

        let visible_route = self
            .window_active
            .then(|| self.active_agent_route(cx))
            .flatten();
        for notification in self.agent_monitor.pending_native_notifications() {
            if !request_native_delivery(visible_route.as_ref(), &notification.route) {
                self.acknowledge_notification(&notification.route, &notification.id, cx);

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

            let activation_url = CliAction::FocusNotification {
                route: notification.route.clone(),
                notification_id: notification.id.clone(),
            }
            .to_url();

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

    pub(super) fn agent_routes_in_surface(surface: &TabSurface, cx: &App) -> Vec<AgentRoute> {
        let mut routes: Vec<_> = surface
            .leaves()
            .into_iter()
            .map(|(_, pane)| pane.read(cx).agent_route().clone())
            .collect();

        if let Some(pane) = surface.agent() {
            routes.push(pane.read(cx).agent_route().clone());
        }

        routes
    }

    fn owns_agent_route(&self, route: &AgentRoute, cx: &App) -> bool {
        self.workspaces.all_tabs().any(|tabs| {
            tabs.tabs().iter().any(|tab| {
                Self::agent_routes_in_surface(tab.surface(), cx)
                    .iter()
                    .any(|candidate| candidate == route)
            })
        })
    }

    fn locate_agent_route(&self, route: &AgentRoute, cx: &App) -> Option<AgentRouteLocation> {
        for (workspace_index, summary) in self.workspaces.summaries().iter().enumerate() {
            let tabs = self.workspaces.tabs_of(summary.id)?;

            for (tab_index, tab) in tabs.tabs().iter().enumerate() {
                if let Some(pane) = tab.surface().agent()
                    && pane.read(cx).agent_route() == route
                {
                    return Some(AgentRouteLocation {
                        workspace_id: summary.id,
                        workspace_index,
                        tab_id: tab.id(),
                        tab_index,
                        target: AgentRouteTarget::Agent(pane.clone()),
                    });
                }

                for (pane_id, pane) in tab.surface().leaves() {
                    if pane.read(cx).agent_route() == route {
                        return Some(AgentRouteLocation {
                            workspace_id: summary.id,
                            workspace_index,
                            tab_id: tab.id(),
                            tab_index,
                            target: AgentRouteTarget::Terminal {
                                pane_id,
                                pane: pane.clone(),
                            },
                        });
                    }
                }
            }
        }
        None
    }

    pub(crate) fn focus_notification(
        &mut self,
        route: &AgentRoute,
        notification_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self
            .agent_monitor
            .notification(route)
            .is_some_and(|notification| notification.id == notification_id && !notification.read)
        {
            return false;
        }

        let Some(location) = self.locate_agent_route(route, cx) else {
            return false;
        };

        self.workspaces.activate(location.workspace_index);

        debug_assert_eq!(self.workspaces.active_id(), location.workspace_id);

        self.workspaces
            .active_tabs_mut()
            .activate(location.tab_index);

        debug_assert_eq!(self.workspaces.active_tabs().active_id(), location.tab_id);

        window.activate_window();

        match location.target {
            AgentRouteTarget::Terminal { pane_id, pane } => {
                self.workspaces
                    .active_tabs_mut()
                    .active_mut()
                    .live_mut()
                    .set_focused(pane_id);

                let handle = pane.read(cx).focus.clone();
                window.focus(&handle, cx);
            }
            AgentRouteTarget::Agent(pane) => {
                pane.update(cx, |pane, cx| pane.focus(window, cx));
            }
        }

        self.acknowledge_notification(route, notification_id, cx);

        true
    }

    pub(crate) fn apply_agent_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) -> bool {
        if !self.owns_agent_route(&event.route, cx) {
            return false;
        }

        let mutation = self.agent_monitor.apply(event, time::Instant::now());

        Self::remove_native_notifications(&mutation.removed_notifications);

        if mutation.visible_changed {
            cx.notify();
        }

        self.reschedule_agent_timer(cx);

        self.process_native_notifications(cx);

        true
    }

    pub(super) fn reschedule_agent_timer(&mut self, cx: &mut Context<Self>) {
        self.agent_timer_generation = self.agent_timer_generation.wrapping_add(1);

        let generation = self.agent_timer_generation;

        let Some(deadline) = self.agent_monitor.next_deadline() else {
            return;
        };

        let delay = deadline.saturating_duration_since(time::Instant::now());

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;

            let _ = this.update(cx, |this, cx| {
                if this.agent_timer_generation != generation {
                    return;
                }

                let mutation = this.agent_monitor.process_due(time::Instant::now());

                Self::remove_native_notifications(&mutation.removed_notifications);

                if mutation.visible_changed {
                    cx.notify();
                }

                this.reschedule_agent_timer(cx);
                this.process_native_notifications(cx);
            });
        })
        .detach();
    }

    /// The tab holding this agent pane. A pane knows its route but not its
    /// tab, and the two are only related through the surface the tab owns.
    fn tab_for_agent_pane(&self, pane: &Entity<AgentPane>) -> Option<TabId> {
        self.workspaces
            .all_tabs()
            .flat_map(|tabs| tabs.tabs())
            .find(|tab| tab.surface().agent() == Some(pane))
            .map(|tab| tab.id())
    }

    pub(crate) fn watch_agent_tab(pane: &Entity<AgentPane>, cx: &mut Context<Self>) {
        cx.subscribe(pane, |this, pane, event: &AgentPaneEvent, cx| {
            let route = pane.read(cx).agent_route().clone();
            let mutation = match event {
                AgentPaneEvent::Lifecycle(event) if event.route == route => this
                    .agent_monitor
                    .apply(event.clone(), time::Instant::now()),
                AgentPaneEvent::Lifecycle(_) => return,
                AgentPaneEvent::WorkflowActivity => {
                    // Sticky: a finished run stays reachable, so the control
                    // never goes away once it has appeared. The running count
                    // is read at render time, so this only has to repaint.
                    this.panels.note_workflow_seen();
                    cx.notify();
                    return;
                }
                AgentPaneEvent::BackgroundTaskActivity => {
                    // Sticky: a finished child stays reachable, so the control
                    // never goes away once it has appeared. The running count
                    // is read at render time, so this only has to repaint the
                    // title bar.
                    this.panels
                        .note_background_task_seen(pane.read(cx).background_task_count() > 0);
                    cx.notify();
                    return;
                }
                AgentPaneEvent::ResumeElsewhere { cwd, session_id } => {
                    // Opening a tab needs a window, which an event
                    // subscription has none of; the next render has one.
                    this.pending_agent_resume = Some(PendingAgentResume {
                        profile: pane.read(cx).profile().clone(),
                        cwd: cwd.clone(),
                        session_id: session_id.clone(),
                    });
                    cx.notify();
                    return;
                }
                AgentPaneEvent::TitleSuggested(title) => {
                    // A user-authored rename outranks this, so a tab the user
                    // has named keeps its name.
                    if let Some(tab_id) = this.tab_for_agent_pane(&pane)
                        && let Some(tabs) = this.workspaces.tab_manager_for_mut(tab_id)
                        && tabs.set_title(tab_id, title.clone())
                    {
                        cx.notify();
                    }
                    return;
                }
                AgentPaneEvent::CloseRequested => {
                    // Same reason as the resume above: closing a tab needs a
                    // window, and the next render has one.
                    this.pending_agent_close = this.tab_for_agent_pane(&pane);
                    cx.notify();
                    return;
                }
                AgentPaneEvent::Interrupted => {
                    this.agent_monitor.interrupt(&route, time::Instant::now())
                }
            };

            Self::remove_native_notifications(&mutation.removed_notifications);

            if mutation.visible_changed {
                cx.notify();
            }

            this.reschedule_agent_timer(cx);
            this.process_native_notifications(cx);
        })
        .detach();
    }
}
