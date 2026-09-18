use gpui::App;
use nmt_agent::chat::ThreadSettings;
use nmt_agent::session::RecoveryIdentity;
use nmt_agent::session::lifecycle::Status;
use nmt_agent::session::team_capabilities::{ModeratorAdmission, RecoveredTeamTurn};
use nmt_agent::team::attempt::{AttemptState, BudgetScope};
use nmt_agent::team::discussion::{DiscussionState, PauseReason};
use nmt_agent::team::model::{AttemptId, InteractionId, MemberId};
use nmt_agent::team::session::{AttemptEventKey, TeamError, TeamSession};

use crate::agent_tab::execution::ExecutionSignal;
use crate::agent_tab::team::TeamCommand;
use crate::agent_tab::team::dispatch::{CONTEXT_LIMITS, WorkStatus, work_status};
use crate::agent_tab::team::events::DecisionArguments;
use crate::agent_tab::team::member_host::MemberHost;

pub(super) struct MemberSnapshot {
    pub id: MemberId,
    pub active: Option<AttemptId>,
    pub interaction: Option<InteractionId>,
    pub ready_epoch: Option<u64>,
    pub start_failure: Option<String>,
    work: WorkStatus,
    status: Status,
    suspended: bool,
    epoch: u64,
    settings: ThreadSettings,
    provider: Option<RecoveryIdentity>,
    capabilities: ModeratorAdmission,
    recovered: Vec<RecoveredTeamTurn>,
}

impl MemberSnapshot {
    pub(super) fn capture(id: MemberId, host: &MemberHost, cx: &App) -> Self {
        let session = host.owner.session().read(cx);
        let work = work_status(session);
        let state = session.controller.borrow();
        let epoch = state.runtime().epoch();
        let backend = state.runtime().backend();

        Self {
            id,
            active: host.active,
            interaction: host.interaction,
            ready_epoch: host.ready_epoch,
            start_failure: state.runtime().start_failure().map(str::to_owned),
            work,
            status: state.runtime().status(),
            suspended: state.runtime().update_suspension().is_some(),
            epoch,
            settings: state.controls.settings.clone(),
            provider: backend.and_then(|backend| backend.recovery_identity()),
            capabilities: backend
                .map(|backend| backend.team_capabilities(session.kind, epoch))
                .unwrap_or_else(|| ModeratorAdmission::unverified(session.kind)),
            recovered: backend
                .map(|backend| backend.team_recovered_turns().to_vec())
                .unwrap_or_default(),
        }
    }
}

pub(super) fn refresh(
    session: &mut TeamSession,
    members: &mut [MemberSnapshot],
) -> Result<Vec<AttemptId>, TeamError> {
    for member in members.iter_mut() {
        if let Some(id) = member.active {
            let attempt = session
                .store()
                .room()
                .attempts()
                .iter()
                .find(|attempt| attempt.id == id)
                .ok_or(TeamError::Unavailable)?;

            if !matches!(
                attempt.state,
                AttemptState::Sending | AttemptState::Accepted | AttemptState::Uncertain
            ) && member.work == WorkStatus::default()
            {
                member.active = None;
            }
        }

        let discussions: Vec<_> = session
            .store()
            .room()
            .discussions()
            .iter()
            .filter(|discussion| discussion.state() != DiscussionState::Completed)
            .map(|discussion| discussion.id())
            .collect();

        if member.work.interaction && member.interaction.is_none() {
            let interaction = InteractionId::new();

            for id in &discussions {
                session.pause_discussion(*id, PauseReason::Interaction(interaction))?;
            }

            member.interaction = Some(interaction);
        } else if !member.work.interaction
            && let Some(interaction) = member.interaction.take()
        {
            for id in &discussions {
                session.resolve_pause(*id, &PauseReason::Interaction(interaction))?;
            }
        }

        if let Some(provider) = &member.provider {
            let recorded = session
                .store()
                .room()
                .member(member.id)
                .ok_or(TeamError::Unavailable)?;

            let registered =
                recorded.moderator_registered() || member.capabilities.check(member.epoch).is_ok();

            session.record_provider_identity(member.id, &provider.id, registered)?;
        }

        if member.suspended {
            for id in &discussions {
                session.pause_discussion(*id, PauseReason::Maintenance(member.id.to_string()))?;
            }

            member.ready_epoch = None;
        }

        match member.status {
            Status::Idle if !member.suspended => {
                for id in &discussions {
                    session.resolve_pause(*id, &PauseReason::Maintenance(member.id.to_string()))?;
                }
            }
            Status::Exited => {
                if let Some(attempt) = member.active.and_then(|id| {
                    session
                        .store()
                        .room()
                        .attempts()
                        .iter()
                        .find(|attempt| attempt.id == id)
                }) && matches!(
                    attempt.state,
                    AttemptState::Sending | AttemptState::Accepted
                ) {
                    session.fail_attempt(
                        AttemptEventKey {
                            attempt: attempt.id,
                            member: member.id,
                            backend_generation: attempt.intent.backend_generation,
                        },
                        true,
                    )?;
                }

                session.member_unavailable(member.id)?;

                member.ready_epoch = None;
            }
            Status::Starting | Status::Running | Status::Idle => {}
        }
    }

    let pending: Vec<_> = session.pending_recovery().cloned().collect();

    for attempt in pending {
        let Some(member) = members
            .iter()
            .find(|member| member.id == attempt.intent.recipient)
        else {
            continue;
        };

        if member.work != WorkStatus::default() || member.status != Status::Idle {
            continue;
        }

        let Some(provider) = &member.provider else {
            continue;
        };

        let Some(saved) = session.store().room().member(member.id) else {
            continue;
        };

        if saved.profile().kind != provider.kind
            || saved.provider_id() != Some(provider.id.as_str())
        {
            continue;
        }

        let Some(turn) = member
            .recovered
            .iter()
            .find(|turn| attempt.provider_turn.as_deref() == Some(turn.id.as_str()))
        else {
            continue;
        };

        let key = AttemptEventKey {
            attempt: attempt.id,
            member: member.id,
            backend_generation: attempt.intent.backend_generation,
        };

        session.accept_attempt(key, &turn.id)?;
        session.complete_reply(key, &turn.id, turn.text.clone())?;
    }

    if session.pending_recovery().next().is_some() {
        return Ok(Vec::new());
    }

    for member in members.iter_mut() {
        if member.status != Status::Idle || member.suspended {
            continue;
        }

        let recorded = session
            .store()
            .room()
            .member(member.id)
            .ok_or(TeamError::Unavailable)?;

        if *recorded.settings() != member.settings {
            session.set_member_settings(member.id, member.settings.clone())?;
        }

        if member.ready_epoch != Some(member.epoch) {
            session.member_ready(member.id, member.epoch, member.capabilities.clone())?;

            member.ready_epoch = Some(member.epoch);
        }
    }

    let discussion = session
        .store()
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

    if let Some(id) = discussion
        && members
            .iter()
            .all(|member| member.active.is_none() && member.work == WorkStatus::default())
    {
        session.advance_discussion(id, &CONTEXT_LIMITS)?;
    }

    Ok(session
        .store()
        .room()
        .attempts()
        .iter()
        .filter(|attempt| attempt.state == AttemptState::Reserved)
        .map(|attempt| attempt.id)
        .collect())
}

pub(super) fn command(
    session: &mut TeamSession,
    command: TeamCommand,
    busy: &[MemberId],
) -> Result<(), TeamError> {
    match command {
        TeamCommand::Continue(_) | TeamCommand::Exclude(_) if !busy.is_empty() => {
            return Err(TeamError::Busy);
        }
        TeamCommand::Skip { .. } if !busy.is_empty() => return Err(TeamError::Unresolved),
        TeamCommand::ChangeMode { discussion, .. } if !busy.is_empty() => {
            session.pause_discussion(discussion, PauseReason::ModeChange)?;

            return Err(TeamError::Busy);
        }
        TeamCommand::Finish(discussion) if !busy.is_empty() => {
            session.pause_discussion(discussion, PauseReason::User)?;

            return Err(TeamError::Unresolved);
        }
        TeamCommand::AbandonRestored(id) => {
            let attempt = session
                .pending_recovery()
                .find(|attempt| attempt.id == id)
                .ok_or(TeamError::Unresolved)?;

            if busy.contains(&attempt.intent.recipient) {
                return Err(TeamError::Busy);
            }

            session.abandon_restored_attempt(id)?;
        }
        TeamCommand::Direct { input, recipients } => {
            session.direct_request(input, recipients, &CONTEXT_LIMITS)?;
        }
        TeamCommand::Start {
            input,
            participants,
            mode,
        } => {
            session.start_discussion(input, participants, mode)?;
        }
        TeamCommand::Correction(input) => {
            session.record_user_input(input)?;
        }
        TeamCommand::Pause(id) => {
            session.pause_discussion(id, PauseReason::User)?;
        }
        TeamCommand::Continue(id) => session.continue_discussion(id)?,
        TeamCommand::AddTurns { discussion, count } => session.add_turns(discussion, count)?,
        TeamCommand::Finish(id) => session.finish_with_report(id)?,
        TeamCommand::Skip {
            discussion,
            operation,
        } => session.skip_arrangement(discussion, operation)?,
        TeamCommand::ChangeMode { discussion, mode } => session.change_mode(discussion, mode)?,
        TeamCommand::AutomaticSummaries(enabled) => session.set_automatic_summaries(enabled)?,
        TeamCommand::MemberSettings { member, settings } => {
            session.set_member_settings(member, settings)?
        }
        TeamCommand::Exclude(member) => session.exclude_member(member)?,
        TeamCommand::Stop(_) => {
            let discussions: Vec<_> = session
                .store()
                .room()
                .discussions()
                .iter()
                .map(|discussion| discussion.id())
                .collect();

            for discussion in discussions {
                session.pause_discussion(discussion, PauseReason::User)?;
            }
        }
    }

    Ok(())
}

pub(super) fn execution(
    session: &mut TeamSession,
    key: AttemptEventKey,
    signal: ExecutionSignal,
) -> Result<bool, TeamError> {
    let Some(attempt) = session
        .store()
        .room()
        .attempts()
        .iter()
        .find(|attempt| attempt.id == key.attempt)
        .cloned()
    else {
        return Ok(false);
    };

    if key.backend_generation != attempt.intent.backend_generation {
        return Ok(false);
    }

    match signal {
        ExecutionSignal::Accepted { id, .. } => {
            session.accept_attempt(key, &id)?;
        }
        ExecutionSignal::Finished {
            id, error, text, ..
        } => {
            if attempt.provider_turn.as_deref() != Some(id.as_str()) {
                return Ok(false);
            }

            if error.is_some() {
                session.fail_attempt(key, false)?;
            } else {
                session.complete_reply(key, &id, text)?;
            }
        }
        ExecutionSignal::Decision { request, .. } => {
            if attempt.provider_turn.as_deref() != Some(request.provider_turn.as_str()) {
                return Ok(false);
            }

            let result = if let Some((stage, operation, action)) =
                serde_json::from_value::<DecisionArguments>(request.arguments)
                    .ok()
                    .and_then(DecisionArguments::moderator_action)
            {
                session.moderator_decision(key, stage, operation, action)
            } else {
                Ok(false)
            };

            let accepted = matches!(result, Ok(true));

            if !accepted && let BudgetScope::Discussion(discussion) = attempt.intent.budget {
                session.pause_discussion(
                    discussion,
                    PauseReason::InvalidModeration(attempt.intent.operation),
                )?;
            }

            return result;
        }
    }

    Ok(false)
}
