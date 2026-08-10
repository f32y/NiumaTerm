use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::AgentRoute;
use crate::event::{
    AgentEvent, AgentEventKind, AgentOwner, AgentRuntimeStatus, normalize_body, normalize_title,
};
use crate::process::agent_process;

#[derive(Clone, Debug)]
pub struct PendingCompletion {
    pub owner: AgentOwner,
    pub turn_generation: u64,
    pub deadline: Instant,
    title: String,
    body: String,
}

#[derive(Clone, Debug)]
pub struct AgentPaneState {
    pub current_owner: Option<AgentOwner>,
    pub turn_generation: u64,
    pub status: AgentRuntimeStatus,
    pub has_work_evidence: bool,
    pub state_started_at: Instant,
    pub updated_at: Instant,
    pub pending_completion: Option<PendingCompletion>,
    notification_generation: u64,
}

impl AgentPaneState {
    fn new(now: Instant) -> Self {
        Self {
            current_owner: None,
            turn_generation: 0,
            status: AgentRuntimeStatus::Idle,
            has_work_evidence: false,
            state_started_at: now,
            updated_at: now,
            pending_completion: None,
            notification_generation: 0,
        }
    }

    fn set_status(&mut self, status: AgentRuntimeStatus, now: Instant) -> bool {
        let changed = self.status != status;

        if changed {
            self.status = status;
            self.state_started_at = now;
        }

        self.updated_at = now;

        changed
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentNotification {
    pub id: String,
    pub route: AgentRoute,
    pub title: String,
    pub body: String,
    pub order: u64,
    pub read: bool,
    pub native_tag: String,
    pub native_group: String,
    pub native_requested: bool,
}

#[derive(Clone, Debug, Default)]
pub struct MonitorMutation {
    pub visible_changed: bool,
    pub removed_notifications: Vec<AgentNotification>,
}

impl MonitorMutation {
    fn merge(&mut self, mut other: Self) {
        self.visible_changed |= other.visible_changed;

        self.removed_notifications
            .append(&mut other.removed_notifications);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProjection {
    pub status: AgentRuntimeStatus,
    pub unread_count: usize,
    pub latest_unread_text: Option<String>,
}

pub struct AgentMonitor {
    process_instance: String,
    panes: HashMap<AgentRoute, AgentPaneState>,
    notifications: HashMap<AgentRoute, AgentNotification>,
    next_notification_order: u64,
}

impl AgentMonitor {
    pub fn new(process_instance: impl Into<String>) -> Self {
        Self {
            process_instance: process_instance.into(),
            panes: HashMap::new(),
            notifications: HashMap::new(),
            next_notification_order: 0,
        }
    }

    pub fn register_route(&mut self, route: AgentRoute, now: Instant) -> bool {
        if self.panes.contains_key(&route) {
            false
        } else {
            self.panes.insert(route, AgentPaneState::new(now));
            true
        }
    }

    #[cfg(test)]
    pub fn pane(&self, route: &AgentRoute) -> Option<&AgentPaneState> {
        self.panes.get(route)
    }

    pub fn notification(&self, route: &AgentRoute) -> Option<&AgentNotification> {
        self.notifications.get(route)
    }

    pub fn pending_native_notifications(&self) -> Vec<AgentNotification> {
        self.notifications
            .values()
            .filter(|notification| !notification.read && !notification.native_requested)
            .cloned()
            .collect()
    }

    pub fn notifications(&self) -> Vec<AgentNotification> {
        self.notifications.values().cloned().collect()
    }

    pub fn mark_native_requested(&mut self, route: &AgentRoute, notification_id: &str) -> bool {
        let Some(notification) = self.notifications.get_mut(route) else {
            return false;
        };

        if notification.id != notification_id || notification.read || notification.native_requested
        {
            return false;
        }

        notification.native_requested = true;

        true
    }

    pub fn apply(&mut self, event: AgentEvent, now: Instant) -> MonitorMutation {
        if !self.panes.contains_key(&event.route) {
            return MonitorMutation::default();
        }

        let route = event.route.clone();

        match event.kind {
            AgentEventKind::SessionStarted => {
                let state = self.panes.get_mut(&route).expect("live route");

                // Session start alone is not evidence of a running turn; it
                // only refreshes pane liveness.
                if state.current_owner.is_none() || state.status == AgentRuntimeStatus::Idle {
                    state.updated_at = now;
                }

                MonitorMutation::default()
            }
            AgentEventKind::PromptSubmitted => {
                let owner = event.owner().expect("validated prompt has turn id");
                let state = self.panes.get_mut(&route).expect("live route");

                let same_turn = state.current_owner.as_ref() == Some(&owner);
                if !same_turn {
                    state.turn_generation = state.turn_generation.wrapping_add(1).max(1);
                }

                state.current_owner = Some(owner);
                state.has_work_evidence = true;
                state.pending_completion = None;

                let status_changed = state.set_status(AgentRuntimeStatus::Running, now);

                let mut mutation = self.remove_notification(&route);

                mutation.visible_changed |= status_changed || !same_turn;

                mutation
            }
            AgentEventKind::ToolStarted | AgentEventKind::ToolFinished => {
                let Some(owner) = event.owner() else {
                    return MonitorMutation::default();
                };

                let state = self.panes.get_mut(&route).expect("live route");
                if state.current_owner.as_ref() != Some(&owner) {
                    return MonitorMutation::default();
                }
                state.has_work_evidence = true;
                state.pending_completion = None;

                let visible_changed = state.set_status(AgentRuntimeStatus::Running, now);
                MonitorMutation {
                    visible_changed,
                    ..MonitorMutation::default()
                }
            }
            AgentEventKind::PermissionRequested => {
                let Some(owner) = event.owner() else {
                    return MonitorMutation::default();
                };

                let state = self.panes.get_mut(&route).expect("live route");
                if state.current_owner.as_ref() != Some(&owner) || !state.has_work_evidence {
                    return MonitorMutation::default();
                }

                state.pending_completion = None;

                let status_changed = state.set_status(AgentRuntimeStatus::NeedsInput, now);
                let mut mutation = self.create_notification(&route, event.title, event.body);
                mutation.visible_changed |= status_changed;

                mutation
            }
            AgentEventKind::Stopped => {
                let Some(owner) = event.owner() else {
                    return MonitorMutation::default();
                };

                let state = self.panes.get_mut(&route).expect("live route");
                if state.current_owner.as_ref() != Some(&owner)
                    || !state.has_work_evidence
                    || state.status == AgentRuntimeStatus::Idle
                {
                    return MonitorMutation::default();
                }

                let generation = state.turn_generation;

                if state.pending_completion.as_ref().is_none_or(|pending| {
                    pending.owner != owner || pending.turn_generation != generation
                }) {
                    state.pending_completion = Some(PendingCompletion {
                        owner,
                        turn_generation: generation,
                        deadline: now + COMPLETION_QUIET_WINDOW,
                        title: event.title,
                        body: event.body,
                    });
                }
                MonitorMutation::default()
            }
        }
    }

    pub fn process_due(&mut self, now: Instant) -> MonitorMutation {
        let routes: Vec<_> = self.panes.keys().cloned().collect();

        let mut result = MonitorMutation::default();

        for route in routes {
            let completion = self
                .panes
                .get(&route)
                .and_then(|state| state.pending_completion.clone())
                .filter(|pending| pending.deadline <= now);

            if let Some(pending) = completion {
                let commit = self.panes.get(&route).is_some_and(|state| {
                    state.current_owner.as_ref() == Some(&pending.owner)
                        && state.turn_generation == pending.turn_generation
                        && state.has_work_evidence
                        && state.status != AgentRuntimeStatus::Idle
                });

                let state = self.panes.get_mut(&route).expect("route still registered");

                state.pending_completion = None;

                if commit {
                    state.has_work_evidence = false;

                    let status_changed = state.set_status(AgentRuntimeStatus::Idle, now);

                    let mut mutation =
                        self.create_notification(&route, pending.title, pending.body);
                    mutation.visible_changed |= status_changed;

                    result.merge(mutation);
                }
            }

            let stale = self.panes.get(&route).is_some_and(|state| {
                matches!(
                    state.status,
                    AgentRuntimeStatus::Running | AgentRuntimeStatus::NeedsInput
                ) && state.updated_at + ACTIVE_STATE_STALE_AFTER <= now
            });

            if stale {
                let state = self.panes.get_mut(&route).expect("route still registered");

                state.pending_completion = None;
                state.has_work_evidence = false;

                result.visible_changed |= state.set_status(AgentRuntimeStatus::Idle, now);
            }
        }
        result
    }

    pub fn notify(&mut self, route: &AgentRoute, title: &str, body: &str) -> MonitorMutation {
        if !self.panes.contains_key(route) {
            return MonitorMutation::default();
        }
        self.create_notification(route, normalize_title(title), normalize_body(body))
    }

    pub fn interrupt(&mut self, route: &AgentRoute, now: Instant) -> MonitorMutation {
        let Some(state) = self.panes.get_mut(route) else {
            return MonitorMutation::default();
        };

        if state.status == AgentRuntimeStatus::Idle {
            return MonitorMutation::default();
        }

        state.current_owner = None;
        state.has_work_evidence = false;
        state.pending_completion = None;

        let status_changed = state.set_status(AgentRuntimeStatus::Idle, now);

        let mut mutation = self.remove_notification(route);

        mutation.visible_changed |= status_changed;

        mutation
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.panes
            .values()
            .flat_map(|state| {
                let completion = state.pending_completion.as_ref().map(|p| p.deadline);
                let stale = matches!(
                    state.status,
                    AgentRuntimeStatus::Running | AgentRuntimeStatus::NeedsInput
                )
                .then_some(state.updated_at + ACTIVE_STATE_STALE_AFTER);
                completion.into_iter().chain(stale)
            })
            .min()
    }

    pub fn acknowledge(&mut self, route: &AgentRoute, notification_id: &str) -> MonitorMutation {
        let Some(notification) = self.notifications.get_mut(route) else {
            return MonitorMutation::default();
        };

        if notification.id != notification_id || notification.read {
            return MonitorMutation::default();
        }

        notification.read = true;

        MonitorMutation {
            visible_changed: true,
            removed_notifications: vec![notification.clone()],
            ..MonitorMutation::default()
        }
    }

    pub fn remove_route(&mut self, route: &AgentRoute) -> MonitorMutation {
        let removed_state = self.panes.remove(route).is_some();

        let mut mutation = self.remove_notification(route);

        mutation.visible_changed |= removed_state;

        mutation
    }

    pub fn project<'a>(&self, routes: impl IntoIterator<Item = &'a AgentRoute>) -> AgentProjection {
        let mut status = AgentRuntimeStatus::Idle;
        let mut unread_count = 0;
        let mut latest: Option<&AgentNotification> = None;

        for route in routes {
            if let Some(state) = self.panes.get(route) {
                status = higher_status(status, state.status);
            }

            if let Some(notification) = self.notifications.get(route).filter(|n| !n.read) {
                unread_count += 1;

                if latest.is_none_or(|current| notification.order > current.order) {
                    latest = Some(notification);
                }
            }
        }
        AgentProjection {
            status,
            unread_count,
            latest_unread_text: latest.map(|notification| {
                if notification.body.is_empty() {
                    notification.title.clone()
                } else {
                    notification.body.clone()
                }
            }),
        }
    }

    fn create_notification(
        &mut self,
        route: &AgentRoute,
        title: String,
        body: String,
    ) -> MonitorMutation {
        let state = self.panes.get_mut(route).expect("live route");

        state.notification_generation = state.notification_generation.wrapping_add(1).max(1);
        self.next_notification_order = self.next_notification_order.wrapping_add(1).max(1);

        // Process-global on purpose, despite making the reducer impure: the
        // native_tag derived from it keys Windows toast replacement, and tags
        // must be unique across every AgentMonitor instance in the process
        // (one per window), which a per-monitor counter cannot guarantee.
        let process_order = agent_process().next_notification_counter();

        let id = format!(
            "{}:{}:{}:{process_order}",
            self.process_instance, route.0, state.notification_generation,
        );

        let notification = AgentNotification {
            id: id.clone(),
            route: route.clone(),
            title,
            body,
            order: self.next_notification_order,
            read: false,
            native_tag: format!("{process_order:016x}"),
            native_group: "NiumaTerm".into(),
            native_requested: false,
        };

        let removed = self.notifications.insert(route.clone(), notification);

        MonitorMutation {
            visible_changed: true,
            removed_notifications: removed.into_iter().collect(),
        }
    }

    fn remove_notification(&mut self, route: &AgentRoute) -> MonitorMutation {
        let removed_notifications: Vec<_> = self.notifications.remove(route).into_iter().collect();

        MonitorMutation {
            visible_changed: !removed_notifications.is_empty(),
            removed_notifications,
            ..MonitorMutation::default()
        }
    }
}

fn higher_status(left: AgentRuntimeStatus, right: AgentRuntimeStatus) -> AgentRuntimeStatus {
    fn priority(status: AgentRuntimeStatus) -> u8 {
        match status {
            AgentRuntimeStatus::Idle => 0,
            AgentRuntimeStatus::Running => 1,
            AgentRuntimeStatus::NeedsInput => 2,
        }
    }

    if priority(right) > priority(left) {
        right
    } else {
        left
    }
}

pub fn request_native_delivery(
    exact_visible_route: Option<&AgentRoute>,
    notification_route: &AgentRoute,
) -> bool {
    exact_visible_route != Some(notification_route)
}

pub fn exact_window_is_active(
    gpui_active: bool,
    foreground_matches_window: bool,
    foreground_minimized: bool,
) -> bool {
    gpui_active && foreground_matches_window && !foreground_minimized
}

pub const COMPLETION_QUIET_WINDOW: Duration = Duration::from_millis(1_500);
pub const ACTIVE_STATE_STALE_AFTER: Duration = Duration::from_secs(30 * 60);
