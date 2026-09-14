use nmt_agent::session::lifecycle::Status;
use nmt_agent::team::context::ContextLimits;
use nmt_agent::team::execution_slots::WorkStatus;

use crate::agent_tab::execution::AgentSession;

pub(super) const CONTEXT_LIMITS: ContextLimits = ContextLimits {
    max_bytes: 96_000,
    recent_messages: 6,
};

pub(super) fn work_status(session: &AgentSession) -> WorkStatus {
    let state = session.controller.borrow();

    WorkStatus {
        foreground: matches!(state.runtime.status(), Status::Running | Status::Starting)
            || state
                .runtime
                .backend()
                .is_some_and(|backend| backend.has_active_operation()),
        background: state.background_activity().1,
        interaction: state.input.waiting(),
        uncertain: false,
    }
}
