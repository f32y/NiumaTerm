use gpui::App;
use nmt_agent::session::lifecycle::Status;
use nmt_agent::team::session::{AttemptEventKey, TeamError};

use crate::agent_tab::team::TeamRuntime;
use crate::agent_tab::team::dispatch::work_status;

impl TeamRuntime {
    pub(super) fn recover_completed_replies(&mut self, cx: &App) -> Result<(), TeamError> {
        let pending: Vec<_> = self.session.pending_recovery().cloned().collect();

        for attempt in pending {
            let Some(host) = self.hosts.get(&attempt.intent.recipient) else {
                continue;
            };

            let session = host.owner.session().read(cx);

            if work_status(session) != Default::default() {
                continue;
            }

            let state = session.controller.borrow();

            if state.runtime.status() != Status::Idle {
                continue;
            }

            let Some(backend) = state.runtime.backend() else {
                continue;
            };

            let Some(identity) = backend.recovery_identity() else {
                continue;
            };

            let Some(member) = self.room().member(attempt.intent.recipient) else {
                continue;
            };

            if member.profile().kind != identity.kind
                || member.provider_id() != Some(identity.id.as_str())
            {
                continue;
            }

            let Some(turn) = backend
                .team_recovered_turns()
                .iter()
                .find(|turn| attempt.provider_turn.as_deref() == Some(turn.id.as_str()))
            else {
                continue;
            };

            let key = AttemptEventKey {
                attempt: attempt.id,
                member: attempt.intent.recipient,
                ownership: attempt.intent.ownership,
                backend_generation: attempt.intent.backend_generation,
            };

            self.session.accept_attempt(key, &turn.id)?;

            self.session
                .complete_reply(key, &turn.id, turn.text.clone(), Default::default())?;
        }

        Ok(())
    }
}
