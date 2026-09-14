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

    pub(super) fn process_due(&mut self, generation: u64) -> Option<MonitorMutation> {
        if self.agent_timer_generation != generation {
            return None;
        }

        Some(self.agent_monitor.process_due(time::Instant::now()))
    }
}
