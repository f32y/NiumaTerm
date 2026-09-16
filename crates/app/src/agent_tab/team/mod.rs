pub use crate::agent_tab::team::controls::TeamCommand;
pub use crate::agent_tab::team::view::TeamPane;

mod controls;
mod dispatch;
mod events;
mod member_host;
mod view;

use std::collections::BTreeMap;
use std::path::Path;

use gpui::{App, AppContext as _, Context, Entity};
use nmt_agent::AgentWorkspace;
use nmt_agent::session::lifecycle::Status;
use nmt_agent::session::team_capabilities::{ModeratorAdmission, TeamLaunch};
use nmt_agent::session::{AgentKind, RecoveryIdentity};
use nmt_agent::team::attempt::{AttemptState, BudgetScope, Invocation};
use nmt_agent::team::discussion::{DiscussionState, PauseReason};
use nmt_agent::team::member::MemberConfig;
use nmt_agent::team::model::{AttemptId, InteractionId, MemberId, RoomId};
use nmt_agent::team::room::Room;
use nmt_agent::team::session::{AttemptEventKey, TeamError, TeamSession};
use nmt_config::profile::AgentProfile;

use crate::agent_tab::execution::{AgentSession, ExecutionSignal, SessionOwner};
use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::team::dispatch::{CONTEXT_LIMITS, WorkStatus, work_status};
use crate::agent_tab::team::events::DecisionArguments;
use crate::agent_tab::team::member_host::MemberHost;

/// Session owners stay alive when the Team view is hidden. Provider events
/// update the durable room before another arrangement becomes eligible.
pub struct TeamRuntime {
    session: TeamSession,
    hosts: BTreeMap<MemberId, MemberHost>,
    error: Option<String>,
    scheduled: bool,
    closed: bool,
}

impl Drop for TeamRuntime {
    fn drop(&mut self) {
        if !self.closed
            && let Err(error) = self.session.close()
        {
            tracing::warn!("could not save closed Team: {error}");
        }
    }
}

impl TeamRuntime {
    pub fn create(
        data_directory: &Path,
        workspace: AgentWorkspace,
        cx: &mut App,
    ) -> Result<Entity<Self>, TeamError> {
        let session = TeamSession::create(data_directory, Room::new(workspace))?;

        Ok(cx.new(|_| Self::new(session)))
    }

    pub fn open(
        data_directory: &Path,
        id: RoomId,
        cx: &mut App,
    ) -> Result<Entity<Self>, TeamError> {
        let session = TeamSession::open(data_directory, id)?;
        let entity = cx.new(|_| Self::new(session));

        entity.update(cx, |this, cx| {
            let members: Vec<_> = this.room().members().iter().filter(|member| !member.excluded()).cloned().collect();

            for member in members {
                let profile = cx.global::<AgentSettings>().profiles.iter().find(|profile| profile.kind == member.profile().kind && profile.name == member.profile().name).cloned();

                match profile {
                    Some(profile) => this.attach_member(member.id(), profile, cx),
                    None => { this.error = Some(format!("The profile for {} is unavailable. Restore that profile before continuing.", member.name())); }
                }
            }

            this.schedule(cx);
        });

        Ok(entity)
    }

    fn new(session: TeamSession) -> Self {
        Self {
            session,
            hosts: BTreeMap::new(),
            error: None,
            scheduled: false,
            closed: false,
        }
    }

    pub fn room(&self) -> &Room {
        self.session.store().room()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn add_member(
        &mut self,
        profile: AgentProfile,
        config: MemberConfig,
        cx: &mut Context<Self>,
    ) -> Result<MemberId, TeamError> {
        if config.profile.kind != profile.kind || config.profile.name != profile.name {
            return Err(TeamError::Unavailable);
        }

        let id = self.session.add_member(config)?;

        self.attach_member(id, profile, cx);

        self.schedule(cx);

        Ok(id)
    }

    fn attach_member(&mut self, id: MemberId, profile: AgentProfile, cx: &mut Context<Self>) {
        let Some(member) = self.room().member(id) else {
            return;
        };

        let recovery = member
            .provider_id()
            .map(|provider| RecoveryIdentity::new(member.profile().kind, provider));

        let options = TeamLaunch {
            moderator: member.profile().kind == AgentKind::Codex
                && (member.provider_id().is_none() || member.moderator_registered()),
            restore_transcript: true,
        };

        let settings = member.settings().clone();

        let owner = AgentSession::create_team(profile, member.roots().clone(), options, cx);

        self.attach_member_owner(id, owner, cx);

        if let Some(host) = self.hosts.get(&id) {
            host.apply_settings(settings, cx);

            host.start(recovery, cx);
        }
    }

    fn attach_member_owner(&mut self, id: MemberId, owner: SessionOwner, cx: &mut Context<Self>) {
        let session = owner.session().clone();

        let events = cx.subscribe(&session, move |this, _, event, cx| {
            this.on_execution(id, event, cx)
        });

        let changed = cx.observe(&session, move |this, _, cx| this.schedule(cx));

        self.hosts
            .insert(id, MemberHost::new(owner, vec![events, changed]));
    }

    pub fn member_session(&self, member: MemberId) -> Option<&Entity<AgentSession>> {
        self.hosts.get(&member).map(|host| host.owner.session())
    }

    fn schedule(&mut self, cx: &mut Context<Self>) {
        cx.notify();

        if self.scheduled || self.closed {
            return;
        }

        self.scheduled = true;

        cx.spawn(async move |this, cx| {
            let _ = this.update(cx, |this, cx| {
                this.scheduled = false;

                if let Err(error) = this.pump(cx) {
                    this.error = Some(error.to_string());
                }

                cx.notify();
            });
        })
        .detach();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) -> Result<(), TeamError> {
        self.closed = true;

        self.session.close()?;

        for host in self.hosts.values() {
            host.interrupt(cx);

            host.owner.close();
        }

        Ok(())
    }

    pub fn command(
        &mut self,
        command: TeamCommand,
        cx: &mut Context<Self>,
    ) -> Result<(), TeamError> {
        let busy = self.hosts.values().any(|host| host.is_busy(cx));

        match command {
            TeamCommand::Continue(_) | TeamCommand::Exclude(_) if busy => {
                return Err(TeamError::Busy);
            }
            TeamCommand::Skip { .. } if busy => return Err(TeamError::Unresolved),
            TeamCommand::ChangeMode { discussion, .. } if busy => {
                self.session
                    .pause_discussion(discussion, PauseReason::ModeChange)?;

                return Err(TeamError::Busy);
            }
            TeamCommand::Finish(discussion) if busy => {
                self.session
                    .pause_discussion(discussion, PauseReason::User)?;

                return Err(TeamError::Unresolved);
            }
            TeamCommand::AbandonRestored(id) => {
                let attempt = self
                    .session
                    .pending_recovery()
                    .find(|attempt| attempt.id == id)
                    .ok_or(TeamError::Unresolved)?;

                if self
                    .hosts
                    .get(&attempt.intent.recipient)
                    .is_some_and(|host| host.is_busy(cx))
                {
                    return Err(TeamError::Busy);
                }

                self.session.abandon_restored_attempt(id)?;
            }
            TeamCommand::Direct { input, recipients } => {
                self.session
                    .direct_request(input, recipients, &CONTEXT_LIMITS)?;
            }
            TeamCommand::Start {
                input,
                participants,
                mode,
            } => {
                self.session.start_discussion(input, participants, mode)?;
            }
            TeamCommand::Correction(input) => {
                self.session.record_user_input(input)?;
            }
            TeamCommand::Pause(id) => {
                self.session.pause_discussion(id, PauseReason::User)?;
            }
            TeamCommand::Continue(id) => self.session.continue_discussion(id)?,
            TeamCommand::AddTurns { discussion, count } => {
                self.session.add_turns(discussion, count)?
            }
            TeamCommand::Finish(id) => self.session.finish_with_report(id)?,
            TeamCommand::Skip {
                discussion,
                operation,
            } => self.session.skip_arrangement(discussion, operation)?,
            TeamCommand::ChangeMode { discussion, mode } => {
                self.session.change_mode(discussion, mode)?
            }
            TeamCommand::AutomaticSummaries(enabled) => {
                self.session.set_automatic_summaries(enabled)?
            }
            TeamCommand::MemberSettings { member, settings } => {
                self.session.set_member_settings(member, settings)?;

                let settings = self
                    .room()
                    .member(member)
                    .ok_or(TeamError::Unavailable)?
                    .settings()
                    .clone();

                self.hosts
                    .get(&member)
                    .ok_or(TeamError::Unavailable)?
                    .apply_settings(settings, cx);
            }
            TeamCommand::Exclude(member) => self.session.exclude_member(member)?,
            TeamCommand::Stop(member) => {
                let discussions: Vec<_> = self
                    .room()
                    .discussions()
                    .iter()
                    .map(|discussion| discussion.id())
                    .collect();

                for discussion in discussions {
                    self.session
                        .pause_discussion(discussion, PauseReason::User)?;
                }

                self.hosts
                    .get(&member)
                    .ok_or(TeamError::Unavailable)?
                    .interrupt(cx);
            }
        }

        self.error = None;

        self.schedule(cx);

        Ok(())
    }

    fn pump(&mut self, cx: &mut Context<Self>) -> Result<(), TeamError> {
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

        if let Some(id) = discussion
            && !self.hosts.values().any(|host| host.is_busy(cx))
        {
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

            if let Some(attempt_id) = host.active {
                let attempt = self
                    .session
                    .store()
                    .room()
                    .attempts()
                    .iter()
                    .find(|attempt| attempt.id == attempt_id)
                    .ok_or(TeamError::Unavailable)?;

                let unresolved = matches!(
                    attempt.state,
                    AttemptState::Sending | AttemptState::Accepted { .. } | AttemptState::Uncertain
                );

                if !unresolved && work == WorkStatus::default() {
                    host.active = None;
                }
            }

            let discussion_ids: Vec<_> = self
                .session
                .store()
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
                    .store()
                    .room()
                    .member(*id)
                    .ok_or(TeamError::Unavailable)?;

                let registered = member.moderator_registered()
                    || backend
                        .team_capabilities(session.kind, state.runtime.epoch())
                        .check(state.runtime.epoch())
                        .is_ok();

                self.session
                    .record_provider_identity(*id, &provider.id, registered)?;
            }

            if state.runtime.update_suspension().is_some() {
                for discussion in &discussion_ids {
                    self.session
                        .pause_discussion(*discussion, PauseReason::Maintenance(id.to_string()))?;
                }

                host.ready_epoch = None;
            }

            match state.runtime.status() {
                Status::Idle if state.runtime.update_suspension().is_none() => {
                    for discussion in &discussion_ids {
                        self.session.resolve_pause(
                            *discussion,
                            &PauseReason::Maintenance(id.to_string()),
                        )?;
                    }
                }
                Status::Exited => {
                    if let Some(attempt_id) = host.active
                        && let Some(attempt) = self
                            .session
                            .store()
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
                Status::Starting | Status::Running | Status::Idle => {}
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
                .store()
                .room()
                .member(*id)
                .ok_or(TeamError::Unavailable)?;

            if state.controls.settings != *member.settings() {
                self.session
                    .set_member_settings(*id, state.controls.settings.clone())?;
            }

            let epoch = state.runtime.epoch();

            if host.ready_epoch == Some(epoch) {
                continue;
            }

            let capabilities = state
                .runtime
                .backend()
                .map(|backend| backend.team_capabilities(session.kind, epoch))
                .unwrap_or_else(|| ModeratorAdmission::unverified(session.kind));

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

        if host.is_busy(cx) {
            return Err(TeamError::Busy);
        }

        let settings = self
            .room()
            .member(attempt.intent.recipient)
            .map(|member| member.settings().clone())
            .ok_or(TeamError::Unavailable)?;

        let outcome = self
            .session
            .dispatch(id, |intent| host.submit(intent, &settings, cx));

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

    fn on_execution(&mut self, member: MemberId, signal: &ExecutionSignal, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }

        if let Err(error) = self.apply_execution(member, signal, cx) {
            self.error = Some(error.to_string());
        }

        self.schedule(cx);
    }

    fn apply_execution(
        &mut self,
        member: MemberId,
        signal: &ExecutionSignal,
        cx: &mut Context<Self>,
    ) -> Result<(), TeamError> {
        let Some(host) = self.hosts.get(&member) else {
            return Ok(());
        };

        let Some(id) = host.active else { return Ok(()) };

        let Some(attempt) = self
            .room()
            .attempts()
            .iter()
            .find(|attempt| attempt.id == id)
            .cloned()
        else {
            return Ok(());
        };

        let epoch = match signal {
            ExecutionSignal::Accepted { epoch, .. }
            | ExecutionSignal::Finished { epoch, .. }
            | ExecutionSignal::Decision { epoch, .. } => *epoch,
        };

        let key = AttemptEventKey {
            attempt: id,
            member,
            backend_generation: epoch,
        };

        if epoch != attempt.intent.backend_generation {
            return Ok(());
        }

        match signal {
            ExecutionSignal::Accepted { id, .. } => {
                self.session.accept_attempt(key, id)?;
            }
            ExecutionSignal::Finished {
                id, error, text, ..
            } => {
                if attempt.provider_turn.as_deref() != Some(id.as_str()) {
                    return Ok(());
                }

                if let Some(error) = error {
                    self.session.fail_attempt(key, false)?;

                    self.error = Some(error.clone());
                } else {
                    self.session.complete_reply(key, id, text.clone())?;
                }
            }
            ExecutionSignal::Decision { request, .. } => {
                let mut accepted = false;
                let mut failure = None;

                if attempt.provider_turn.as_deref() == Some(request.provider_turn.as_str()) {
                    if let Some((stage, operation, action)) =
                        serde_json::from_value::<DecisionArguments>(request.arguments.clone())
                            .ok()
                            .and_then(DecisionArguments::moderator_action)
                    {
                        match self
                            .session
                            .moderator_decision(key, stage, operation, action)
                        {
                            Ok(result) => accepted = result,
                            Err(error) => failure = Some(error),
                        }
                    }

                    if !accepted
                        && let BudgetScope::Discussion(discussion) = attempt.intent.budget
                        && let Err(error) = self.session.pause_discussion(
                            discussion,
                            PauseReason::InvalidModeration(attempt.intent.operation),
                        )
                    {
                        failure = Some(error);
                    }
                }

                host.respond_decision(request, accepted, cx);

                if let Some(error) = failure {
                    return Err(error);
                }
            }
        }

        Ok(())
    }

    fn recover_completed_replies(&mut self, cx: &App) -> Result<(), TeamError> {
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
                backend_generation: attempt.intent.backend_generation,
            };

            self.session.accept_attempt(key, &turn.id)?;

            self.session
                .complete_reply(key, &turn.id, turn.text.clone())?;
        }

        Ok(())
    }
}
