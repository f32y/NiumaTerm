use gpui::{App, Context};
use nmt_agent::chat::SendOutcome;
use nmt_agent::session::ImageAttachment;
use nmt_agent::session::delivery::Submission;
use nmt_agent::session::lifecycle::Status;
use nmt_agent::session::team_capabilities::TeamCapabilities;
use nmt_agent::team::attempt::{AttemptState, Invocation};
use nmt_agent::team::context::ContextLimits;
use nmt_agent::team::discussion::{DiscussionState, PauseReason};
use nmt_agent::team::execution_slots::{ExecutionKey, WorkStatus};
use nmt_agent::team::identity::{AttemptId, InteractionId};
use nmt_agent::team::session::{AttemptEventKey, TeamError};

use crate::agent_tab::composer::attachments::scratch_dir;
use crate::agent_tab::execution::AgentSession;
use crate::agent_tab::team::TeamRuntime;

pub(super) const CONTEXT_LIMITS: ContextLimits = ContextLimits {
    max_bytes: 96_000,
    recent_messages: 6,
};

impl TeamRuntime {
    pub(super) fn pump(&mut self, cx: &mut Context<Self>) -> Result<(), TeamError> {
        if self.closed {
            return Ok(());
        }

        self.sync_work(cx)?;
        self.recover_completed_replies(cx)?;

        if self.session.pending_recovery().next().is_some() {
            return Ok(());
        }

        self.sync_ready_members(cx)?;

        let discussion = self
            .room()
            .discussions()
            .iter()
            .find(|discussion| {
                matches!(
                    discussion.state(),
                    DiscussionState::Running | DiscussionState::Finishing
                )
            })
            .map(|discussion| discussion.id());

        if let Some(id) = discussion {
            self.session.advance_discussion(id, &CONTEXT_LIMITS)?;
        }

        let pending: Vec<_> = self
            .room()
            .attempts()
            .iter()
            .filter(|attempt| attempt.state == AttemptState::Reserved)
            .map(|attempt| attempt.id)
            .collect();

        for id in pending {
            match self.dispatch(id, cx) {
                Ok(()) | Err(TeamError::Busy | TeamError::Paused | TeamError::Unavailable) => {}
                Err(error) => return Err(error),
            }
        }

        Ok(())
    }

    fn sync_work(&mut self, cx: &App) -> Result<(), TeamError> {
        for (id, host) in &mut self.hosts {
            let session = host.owner.session().read(cx);
            let work = work_status(session);

            let ownership = self
                .session
                .room()
                .member(*id)
                .ok_or(TeamError::Unavailable)?
                .ownership();

            if let Some(attempt_id) = host.active {
                let attempt = self
                    .session
                    .room()
                    .attempts()
                    .iter()
                    .find(|attempt| attempt.id == attempt_id)
                    .ok_or(TeamError::Unavailable)?;

                let unresolved = matches!(
                    attempt.state,
                    AttemptState::Sending | AttemptState::Accepted { .. } | AttemptState::Uncertain
                );

                let key = ExecutionKey {
                    member: *id,
                    ownership,
                    attempt: attempt_id,
                };

                if !unresolved {
                    self.session.update_work(key, work);

                    if work == WorkStatus::default() {
                        host.active = None;
                    }
                }
            }

            let discussion_ids: Vec<_> = self
                .session
                .room()
                .discussions()
                .iter()
                .filter(|discussion| discussion.state() != DiscussionState::Completed)
                .map(|discussion| discussion.id())
                .collect();

            if work.interaction && host.interaction.is_none() {
                let interaction = InteractionId::new();

                for discussion in &discussion_ids {
                    self.session
                        .pause_discussion(*discussion, PauseReason::Interaction(interaction))?;
                }

                host.interaction = Some(interaction);
            } else if !work.interaction
                && let Some(interaction) = host.interaction.take()
            {
                for discussion in &discussion_ids {
                    self.session
                        .resolve_pause(*discussion, &PauseReason::Interaction(interaction))?;
                }
            }

            let state = session.controller.borrow();

            if let Some(backend) = state.runtime.backend()
                && let Some(provider) = backend.recovery_identity()
            {
                let member = self
                    .session
                    .room()
                    .member(*id)
                    .ok_or(TeamError::Unavailable)?;

                let registered = member.moderator_registered()
                    || backend
                        .team_capabilities(session.kind, state.runtime.epoch())
                        .moderation
                        .check(state.runtime.epoch())
                        .is_ok();

                self.session
                    .record_provider_identity(*id, ownership, &provider.id, registered)?;
            }

            if state.runtime.update_suspension().is_some() {
                for discussion in &discussion_ids {
                    self.session
                        .pause_discussion(*discussion, PauseReason::Maintenance(id.to_string()))?;
                }

                host.ready_epoch = None;
            } else if state.runtime.status() == Status::Idle {
                for discussion in &discussion_ids {
                    self.session
                        .resolve_pause(*discussion, &PauseReason::Maintenance(id.to_string()))?;
                }
            }

            if state.runtime.status() == Status::Exited {
                if let Some(attempt_id) = host.active
                    && let Some(attempt) = self
                        .session
                        .room()
                        .attempts()
                        .iter()
                        .find(|attempt| attempt.id == attempt_id)
                    && matches!(
                        attempt.state,
                        AttemptState::Sending | AttemptState::Accepted { .. }
                    )
                {
                    self.session.fail_attempt(
                        AttemptEventKey {
                            attempt: attempt_id,
                            member: *id,
                            ownership,
                            backend_generation: attempt.intent.backend_generation,
                        },
                        true,
                    )?;
                }

                self.session.member_unavailable(*id)?;
                host.ready_epoch = None;

                if let Some(message) = state.runtime.start_failure() {
                    self.error = Some(message.to_owned());
                }
            }
        }

        Ok(())
    }

    fn sync_ready_members(&mut self, cx: &App) -> Result<(), TeamError> {
        for (id, host) in &mut self.hosts {
            let session = host.owner.session().read(cx);
            let state = session.controller.borrow();

            if state.runtime.status() != Status::Idle || state.runtime.update_suspension().is_some()
            {
                continue;
            }

            let member = self
                .session
                .room()
                .member(*id)
                .ok_or(TeamError::Unavailable)?;

            let ownership = member.ownership();

            if state.controls().settings != *member.settings() {
                self.session.set_member_settings(
                    *id,
                    ownership,
                    state.controls().settings.clone(),
                )?;
            }

            let epoch = state.runtime.epoch();

            if host.ready_epoch == Some(epoch) {
                continue;
            }

            let capabilities = state
                .runtime
                .backend()
                .map(|backend| backend.team_capabilities(session.kind, epoch))
                .unwrap_or_else(|| TeamCapabilities::unverified(session.kind));

            self.session.member_ready(*id, epoch, capabilities)?;
            host.ready_epoch = Some(epoch);
        }

        Ok(())
    }

    fn dispatch(&mut self, id: AttemptId, cx: &mut Context<Self>) -> Result<(), TeamError> {
        let attempt = self
            .room()
            .attempts()
            .iter()
            .find(|attempt| attempt.id == id)
            .ok_or(TeamError::Unavailable)?
            .clone();

        if !matches!(attempt.intent.invocation, Invocation::MemberConversation) {
            return Err(TeamError::Unavailable);
        }

        let host = self
            .hosts
            .get(&attempt.intent.recipient)
            .ok_or(TeamError::Unavailable)?;

        if host.active.is_some()
            || work_status(host.owner.session().read(cx)) != WorkStatus::default()
        {
            return Err(TeamError::Busy);
        }

        let execution = host.owner.session().clone();

        let attachments: Vec<_> = attempt
            .intent
            .attachments
            .iter()
            .map(|reference| self.session.read_attachment(reference))
            .collect::<Result<_, _>>()?;

        let settings = self
            .room()
            .member(attempt.intent.recipient)
            .map(|member| member.settings().clone())
            .ok_or(TeamError::Unavailable)?;

        let outcome = self.session.dispatch(id, |intent| {
            execution.update(cx, |session, cx| {
                let mut state = session.controller.borrow_mut();

                if state.runtime.status() != Status::Idle
                    || state.runtime.update_suspension().is_some()
                {
                    return SendOutcome::NotReady;
                }

                let scratch = scratch_dir(session.agent_route().as_str());

                let result = state.submit(
                    intent.prepared_text.clone(),
                    |backend, text| {
                        backend.send_user_message(
                            text,
                            &settings,
                            None,
                            attachments.iter().zip(&intent.attachments).map(
                                |(bytes, reference)| ImageAttachment {
                                    bytes,
                                    media_type: &reference.media_type,
                                },
                            ),
                            &scratch,
                        )
                    },
                    || None,
                );

                cx.notify();

                match result {
                    Ok(Submission::Started { .. }) => SendOutcome::StartedTurn,
                    Ok(Submission::Queued) => SendOutcome::Steered,
                    Ok(Submission::Rejected { message }) => SendOutcome::Rejected { message },
                    Ok(Submission::NotReady) | Err(_) => SendOutcome::NotReady,
                }
            })
        });

        if self.room().attempts().iter().any(|attempt| {
            attempt.id == id
                && matches!(
                    attempt.state,
                    AttemptState::Sending | AttemptState::Uncertain
                )
        }) && let Some(host) = self.hosts.get_mut(&attempt.intent.recipient)
        {
            host.active = Some(id);
        }

        outcome?;

        Ok(())
    }
}

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
