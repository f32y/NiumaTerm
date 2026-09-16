//! Durable room operations and live dispatch readiness owned by one Team.

pub use crate::team::session::dispatch::DispatchError;
pub use crate::team::session::outcomes::AttemptEventKey;

pub(super) mod dispatch;

mod outcomes;
mod planning;

#[cfg(test)]
mod planning_tests;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use thiserror::Error;

use crate::chat::{SendOutcome, ThreadSettings};
use crate::session::team_capabilities::ModeratorAdmission;
use crate::team::attempt::{Attempt, AttemptState, BudgetScope, DispatchIntent, Invocation};
use crate::team::budget::{BudgetError, TurnPurpose};
use crate::team::discussion::{
    ArrangementState, DiscussionError, DiscussionMode, DiscussionState, ModeratorAction,
    ModeratorDecision, PauseReason, PublicSnapshot, Stage, StageKind,
};
use crate::team::member::MemberConfig;
use crate::team::model::{
    AttemptId, ContextError, ContextLimits, DiscussionId, MemberId, MessageId, OperationId, RoomId,
    StageId, UserInput,
};
use crate::team::room::{MemberError, Room};
use crate::team::session::outcomes::{
    abandon, accept, accepts, complete, completes, event_attempt, fail, in_flight,
};
use crate::team::session::planning::{
    DispatchPlan, NextStage, build_intent, next_stage, public_request, stage_purpose,
};
use crate::team::storage::{RoomStore, StorageError};

pub struct TeamSession {
    store: RoomStore,
    readiness: BTreeMap<MemberId, MemberReadiness>,
    restored_uncertainty: BTreeSet<AttemptId>,
}

struct MemberReadiness {
    backend_generation: u64,
    capabilities: ModeratorAdmission,
}

#[derive(Debug, Error)]
pub enum TeamError {
    #[error(transparent)]
    Member(#[from] MemberError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Dispatch(#[from] DispatchError),
    #[error("member is unavailable")]
    Unavailable,
    #[error("discussion is paused")]
    Paused,
    #[error("member is busy")]
    Busy,
    #[error(transparent)]
    Discussion(#[from] DiscussionError),
    #[error(transparent)]
    Context(#[from] ContextError),
    #[error(transparent)]
    Budget(#[from] BudgetError),
    #[error("this arrangement cannot be changed while its work is unresolved")]
    Unresolved,
}

impl TeamSession {
    pub fn store(&self) -> &RoomStore {
        &self.store
    }

    pub fn create(data_directory: &Path, room: Room) -> Result<Self, TeamError> {
        Ok(Self {
            store: RoomStore::create(data_directory, room)?,
            readiness: BTreeMap::new(),
            restored_uncertainty: BTreeSet::new(),
        })
    }

    pub fn open(data_directory: &Path, id: RoomId) -> Result<Self, TeamError> {
        let mut store = RoomStore::open(data_directory, id)?;
        let mut room = store.room().clone();

        for discussion in &mut room.discussions {
            discussion.pause(PauseReason::Reopened);
        }

        for attempt in &mut room.attempts {
            if matches!(
                attempt.state,
                AttemptState::Sending | AttemptState::Accepted { .. }
            ) {
                attempt.state = AttemptState::Uncertain;
            }
        }

        let restored_uncertainty = room
            .attempts
            .iter()
            .filter(|attempt| in_flight(&attempt.state))
            .map(|attempt| attempt.id)
            .collect();

        for discussion in &mut room.discussions {
            for stage in &mut discussion.stages {
                for arrangement in &mut stage.arrangements {
                    if let ArrangementState::Active(id) = arrangement.state {
                        arrangement.state = ArrangementState::Uncertain(id);
                    }
                }
            }
        }

        store.commit(room)?;

        Ok(Self {
            store,
            readiness: BTreeMap::new(),
            restored_uncertainty,
        })
    }

    pub fn member_ready(
        &mut self,
        id: MemberId,
        backend_generation: u64,
        capabilities: ModeratorAdmission,
    ) -> Result<(), TeamError> {
        let member = self.store.room().member(id).ok_or(TeamError::Unavailable)?;

        if member.excluded() {
            return Err(TeamError::Unavailable);
        }

        let mut room = self.store.room().clone();
        let mut changed = false;

        for discussion in &mut room.discussions {
            changed |= discussion.resolve_pause(&PauseReason::MemberUnavailable(id));
        }

        if changed {
            self.store.commit(room)?;
        }

        self.readiness.insert(
            id,
            MemberReadiness {
                backend_generation,
                capabilities,
            },
        );

        Ok(())
    }

    pub(crate) fn reserve_dispatches(
        &mut self,
        intents: Vec<DispatchIntent>,
    ) -> Result<Vec<AttemptId>, TeamError> {
        for intent in &intents {
            self.validate_recipient(intent)?;
        }

        Ok(dispatch::reserve_dispatches(&mut self.store, intents)?)
    }

    pub fn dispatch(
        &mut self,
        id: AttemptId,
        send: impl FnOnce(&DispatchIntent) -> SendOutcome,
    ) -> Result<SendOutcome, TeamError> {
        let attempt = self
            .store
            .room()
            .attempts()
            .iter()
            .find(|attempt| attempt.id == id)
            .ok_or(TeamError::Unavailable)?;

        self.validate_recipient(&attempt.intent)?;

        if let BudgetScope::Discussion(id) = attempt.intent.budget {
            let run = self
                .store
                .room()
                .discussion(id)
                .ok_or(TeamError::Unavailable)?;

            if !run.is_dispatchable() {
                return Err(TeamError::Paused);
            }
        }

        if self.store.room().attempts.iter().any(|active| {
            active.intent.recipient == attempt.intent.recipient && in_flight(&active.state)
        }) {
            return Err(TeamError::Busy);
        }

        Ok(dispatch::dispatch(&mut self.store, id, send)?)
    }

    fn has_live_attempts(&self) -> bool {
        self.store.room().attempts.iter().any(|attempt| {
            in_flight(&attempt.state) && !self.restored_uncertainty.contains(&attempt.id)
        })
    }

    pub fn member_unavailable(&mut self, member: MemberId) -> Result<(), TeamError> {
        self.readiness.remove(&member);

        let mut room = self.store.room().clone();

        for discussion in &mut room.discussions {
            discussion.pause(PauseReason::MemberUnavailable(member));
        }

        self.store.commit(room)?;

        Ok(())
    }

    fn validate_recipient(&self, intent: &DispatchIntent) -> Result<(), TeamError> {
        let readiness = self
            .readiness
            .get(&intent.recipient)
            .ok_or(TeamError::Unavailable)?;

        if readiness.backend_generation != intent.backend_generation {
            return Err(TeamError::Unavailable);
        }

        self.validate_member(intent.recipient)
    }

    pub(super) fn validate_member(&self, id: MemberId) -> Result<(), TeamError> {
        if !self.restored_uncertainty.is_empty() {
            return Err(TeamError::Unavailable);
        }

        let member = self.store.room().member(id).ok_or(TeamError::Unavailable)?;

        self.readiness.get(&id).ok_or(TeamError::Unavailable)?;

        if member.excluded() {
            return Err(TeamError::Unavailable);
        }

        Ok(())
    }

    pub fn start_discussion(
        &mut self,
        input: UserInput,
        participants: Vec<MemberId>,
        mode: DiscussionMode,
    ) -> Result<DiscussionId, TeamError> {
        self.validate_input(&input)?;

        self.validate_mode(mode)?;

        let mut room = self.store.room().clone();

        let id = room
            .create_discussion(input.text.clone(), participants, mode)?
            .id();

        room.input_history.push(input.clone());

        let request = input.clone();

        room.messages.push(public_request(input));

        let discussion = room.discussion_mut(id).ok_or(TeamError::Unavailable)?;

        discussion.state = DiscussionState::Running;
        discussion.request = request;

        self.store.commit(room)?;

        Ok(id)
    }

    pub fn record_user_input(&mut self, input: UserInput) -> Result<MessageId, TeamError> {
        self.validate_input(&input)?;

        let mut room = self.store.room().clone();

        room.input_history.push(input.clone());

        let message = public_request(input);
        let id = message.id;

        room.messages.push(message);

        for discussion in &mut room.discussions {
            discussion.pause(PauseReason::UserInput(id));
        }

        self.store.commit(room)?;

        Ok(id)
    }

    pub fn direct_request(
        &mut self,
        input: UserInput,
        recipients: Vec<MemberId>,
        limits: &ContextLimits,
    ) -> Result<Vec<AttemptId>, TeamError> {
        self.validate_input(&input)?;

        if recipients.is_empty()
            || recipients.iter().copied().collect::<BTreeSet<_>>().len() != recipients.len()
        {
            return Err(TeamError::Unavailable);
        }

        let snapshot = self.store.room().public_snapshot();
        let operation = OperationId::new();

        let mut intents = Vec::new();

        for recipient in recipients {
            intents.push(self.prepare_intent(
                DispatchPlan {
                    recipient,
                    operation,
                    stage: None,
                    budget: BudgetScope::Direct(operation),
                    purpose: TurnPurpose::Response,
                },
                input.clone(),
                &snapshot,
                limits,
            )?);
        }

        self.record_user_input(input)?;

        self.reserve_dispatches(intents)
    }

    /// A stage gets one public boundary before any recipient is dispatched.
    /// Recipients do not see each other's output until the next stage or an
    /// explicit resumed segment, regardless of their completion order.
    pub fn advance_discussion(
        &mut self,
        id: DiscussionId,
        limits: &ContextLimits,
    ) -> Result<Vec<AttemptId>, TeamError> {
        let mut room = self.store.room().clone();

        let snapshot = room.public_snapshot();

        let discussion = room.discussion_mut(id).ok_or(TeamError::Unavailable)?;

        if discussion.state == DiscussionState::Completed {
            return Ok(Vec::new());
        }

        if !discussion.is_dispatchable() {
            return Err(TeamError::Paused);
        }

        self.validate_mode(discussion.mode)?;

        let stage_complete = discussion.stages.last().is_none_or(|stage| {
            stage.arrangements.iter().all(|arrangement| {
                matches!(
                    arrangement.state,
                    ArrangementState::Completed(_)
                        | ArrangementState::Skipped
                        | ArrangementState::Cancelled
                )
            })
        });

        if stage_complete {
            if self.has_live_attempts() {
                return Ok(Vec::new());
            }

            let (kind, recipients) = match next_stage(
                discussion,
                discussion.remaining_non_report_turns(self.store.room().attempts()),
            )? {
                NextStage::Schedule(kind, recipients) => (kind, recipients),
                NextStage::InvalidModeration(operation) => {
                    self.pause_discussion(id, PauseReason::InvalidModeration(operation))?;

                    return Err(TeamError::Paused);
                }
            };

            discussion
                .stages
                .push(Stage::pending(kind, recipients, snapshot));

            self.store.commit(room)?;
        }

        let discussion = self
            .store
            .room()
            .discussion(id)
            .ok_or(TeamError::Unavailable)?;

        let stage = discussion.stages.last().ok_or(TeamError::Unavailable)?;

        let snapshot = stage.segments.last().ok_or(TeamError::Unavailable)?;

        let purpose = stage_purpose(stage.kind);

        let input = discussion.request.clone();

        let mut intents = Vec::new();
        let mut ready = Vec::new();

        for arrangement in &stage.arrangements {
            if arrangement.state != ArrangementState::Pending {
                continue;
            }

            if let Some(attempt) = self.store.room().attempts.iter().find(|attempt| {
                attempt.intent.operation == arrangement.operation
                    && attempt.state == AttemptState::Reserved
            }) {
                ready.push(attempt.id);

                continue;
            }

            let intent = match self.prepare_intent(
                DispatchPlan {
                    recipient: arrangement.recipient,
                    operation: arrangement.operation,
                    stage: Some((stage.id, stage.kind)),
                    budget: BudgetScope::Discussion(id),
                    purpose,
                },
                input.clone(),
                snapshot,
                limits,
            ) {
                Ok(intent) => intent,
                Err(error) => {
                    self.pause_dispatch_error(id, &error)?;

                    return Err(error);
                }
            };

            intents.push(intent);
        }

        if intents.is_empty() {
            return Ok(ready);
        }

        match self.reserve_dispatches(intents) {
            Ok(ids) => {
                ready.extend(ids);

                Ok(ready)
            }
            Err(error) => {
                self.pause_dispatch_error(id, &error)?;

                Err(error)
            }
        }
    }

    fn pause_dispatch_error(
        &mut self,
        id: DiscussionId,
        error: &TeamError,
    ) -> Result<(), TeamError> {
        let reason = match error {
            TeamError::Budget(_) | TeamError::Dispatch(DispatchError::Budget(_)) => {
                PauseReason::Budget
            }
            TeamError::Storage(_) | TeamError::Dispatch(DispatchError::Storage(_)) => {
                PauseReason::Storage
            }
            TeamError::Context(_) => PauseReason::ContextSelection,
            _ => PauseReason::DispatchUnavailable,
        };

        self.pause_discussion(id, reason)?;

        Ok(())
    }

    pub(crate) fn validate_mode(&self, mode: DiscussionMode) -> Result<(), TeamError> {
        let member = self
            .store
            .room()
            .member(mode.report_author())
            .ok_or(TeamError::Unavailable)?;

        if member.excluded {
            return Err(TeamError::Unavailable);
        }

        if let DiscussionMode::Moderated { moderator } = mode {
            let readiness = self
                .readiness
                .get(&moderator)
                .ok_or(TeamError::Unavailable)?;

            readiness
                .capabilities
                .check(readiness.backend_generation)
                .map_err(|_| TeamError::Unavailable)?;
        }

        Ok(())
    }

    fn prepare_intent(
        &self,
        plan: DispatchPlan,
        input: UserInput,
        snapshot: &PublicSnapshot,
        limits: &ContextLimits,
    ) -> Result<DispatchIntent, TeamError> {
        let readiness = self
            .readiness
            .get(&plan.recipient)
            .ok_or(TeamError::Unavailable)?;

        let intent = build_intent(
            self.store.room(),
            plan,
            readiness.backend_generation,
            input,
            snapshot,
            limits,
        )?;

        self.validate_recipient(&intent)?;

        Ok(intent)
    }

    fn validate_input(&self, input: &UserInput) -> Result<(), TeamError> {
        if input.references.iter().any(|id| {
            !self
                .store
                .room()
                .messages
                .iter()
                .any(|message| message.id == *id)
        }) {
            return Err(TeamError::Unavailable);
        }

        Ok(())
    }

    pub fn pause_discussion(
        &mut self,
        id: DiscussionId,
        reason: PauseReason,
    ) -> Result<bool, TeamError> {
        let mut room = self.store.room().clone();

        let changed = room
            .discussion_mut(id)
            .ok_or(TeamError::Unavailable)?
            .pause(reason);

        self.store.commit(room)?;

        Ok(changed)
    }

    pub fn resolve_pause(
        &mut self,
        id: DiscussionId,
        reason: &PauseReason,
    ) -> Result<bool, TeamError> {
        let mut room = self.store.room().clone();

        let changed = room
            .discussion_mut(id)
            .ok_or(TeamError::Unavailable)?
            .resolve_pause(reason);

        self.store.commit(room)?;

        Ok(changed)
    }

    pub fn continue_discussion(&mut self, id: DiscussionId) -> Result<(), TeamError> {
        if self.has_live_attempts() || !self.restored_uncertainty.is_empty() {
            return Err(TeamError::Busy);
        }

        let mut room = self.store.room().clone();

        let index = room
            .discussions
            .iter()
            .position(|run| run.id == id)
            .ok_or(TeamError::Unavailable)?;

        let discussion = &mut room.discussions[index];

        if discussion.state == DiscussionState::Completed {
            return Err(TeamError::Unavailable);
        }

        self.validate_mode(discussion.mode)?;

        discussion.pauses.retain(|reason| {
            !matches!(
                reason,
                PauseReason::User
                    | PauseReason::UserInput(_)
                    | PauseReason::ModeChange
                    | PauseReason::Reopened
                    | PauseReason::Closed
            )
        });

        if !discussion.pauses.is_empty() {
            return Err(TeamError::Paused);
        }

        cancel_pending_reservations(&mut room, id)?;

        let snapshot = room.public_snapshot();
        let discussion = &mut room.discussions[index];

        if let Some(stage) = discussion.stages.last_mut() {
            stage.segments.push(snapshot);
        }

        discussion.state = DiscussionState::Running;

        self.store.commit(room)?;

        Ok(())
    }

    pub fn add_turns(&mut self, id: DiscussionId, turns: u32) -> Result<(), TeamError> {
        let mut room = self.store.room().clone();

        let discussion = room.discussion_mut(id).ok_or(TeamError::Unavailable)?;

        discussion.budget.add_turns(turns)?;

        discussion.resolve_pause(&PauseReason::Budget);

        self.store.commit(room)?;

        Ok(())
    }

    pub fn change_mode(&mut self, id: DiscussionId, mode: DiscussionMode) -> Result<(), TeamError> {
        self.pause_discussion(id, PauseReason::ModeChange)?;

        if self.has_live_attempts() {
            return Err(TeamError::Busy);
        }

        self.validate_mode(mode)?;

        let mut room = self.store.room().clone();

        cancel_pending_reservations(&mut room, id)?;

        let discussion = room.discussion_mut(id).ok_or(TeamError::Unavailable)?;

        discussion.change_mode(mode)?;

        for stage in &mut discussion.stages {
            if matches!(stage.kind, StageKind::ModeratorDecision | StageKind::Report) {
                for arrangement in &mut stage.arrangements {
                    if arrangement.state == ArrangementState::Pending {
                        arrangement.state = ArrangementState::Cancelled;
                    }
                }
            }
        }

        self.store.commit(room)?;

        Ok(())
    }

    pub fn skip_arrangement(
        &mut self,
        id: DiscussionId,
        operation: OperationId,
    ) -> Result<(), TeamError> {
        if self.has_live_attempts() || !self.restored_uncertainty.is_empty() {
            return Err(TeamError::Unresolved);
        }

        let mut room = self.store.room().clone();

        cancel_pending_reservations(&mut room, id)?;

        let discussion = room.discussion_mut(id).ok_or(TeamError::Unavailable)?;

        let arrangement = discussion
            .stages
            .iter_mut()
            .flat_map(|stage| &mut stage.arrangements)
            .find(|entry| entry.operation == operation)
            .ok_or(TeamError::Unavailable)?;

        match arrangement.state {
            ArrangementState::Pending => {}
            ArrangementState::Failed(attempt) => {
                discussion
                    .pauses
                    .remove(&PauseReason::AttemptFailed(attempt));
            }
            _ => return Err(TeamError::Unresolved),
        }

        arrangement.state = ArrangementState::Skipped;

        discussion.pause(PauseReason::User);

        self.store.commit(room)?;

        Ok(())
    }

    pub fn finish_with_report(&mut self, id: DiscussionId) -> Result<(), TeamError> {
        self.pause_discussion(id, PauseReason::User)?;

        if self.has_live_attempts() || !self.restored_uncertainty.is_empty() {
            return Err(TeamError::Unresolved);
        }

        let mut room = self.store.room().clone();

        cancel_pending_reservations(&mut room, id)?;

        let snapshot = room.public_snapshot();

        let discussion = room.discussion_mut(id).ok_or(TeamError::Unavailable)?;

        if discussion.state == DiscussionState::Completed {
            return Err(TeamError::Unavailable);
        }

        for arrangement in discussion
            .stages
            .iter_mut()
            .flat_map(|stage| &mut stage.arrangements)
        {
            if matches!(
                arrangement.state,
                ArrangementState::Pending | ArrangementState::Failed(_)
            ) {
                arrangement.state = ArrangementState::Cancelled;
            }
        }

        discussion.pauses.retain(|reason| {
            !matches!(
                reason,
                PauseReason::Budget | PauseReason::AttemptFailed(_) | PauseReason::User
            )
        });

        let report_author = discussion.mode.report_author();

        discussion.stages.push(Stage::pending(
            StageKind::Report,
            vec![report_author],
            snapshot,
        ));

        discussion.state = if discussion.pauses.is_empty() {
            DiscussionState::Finishing
        } else {
            DiscussionState::Paused
        };

        self.store.commit(room)?;

        Ok(())
    }

    /// Only a provider's identified turn acceptance advances delivered context.
    /// Writing bytes to its transport cannot establish this transition.
    pub fn accept_attempt(
        &mut self,
        key: AttemptEventKey,
        provider_turn: &str,
    ) -> Result<bool, TeamError> {
        let Some(index) = event_attempt(self.store.room(), key) else {
            return Ok(false);
        };

        if !accepts(&self.store.room().attempts[index], provider_turn) {
            return Ok(false);
        }

        let mut room = self.store.room().clone();

        accept(&mut room, index, provider_turn)?;

        self.store.commit(room)?;

        Ok(true)
    }

    pub fn complete_reply(
        &mut self,
        key: AttemptEventKey,
        provider_turn: &str,
        text: String,
    ) -> Result<Option<MessageId>, TeamError> {
        let Some(index) = event_attempt(self.store.room(), key) else {
            return Ok(None);
        };

        if !completes(&self.store.room().attempts[index], provider_turn) {
            return Ok(None);
        }

        let mut room = self.store.room().clone();

        let id = complete(&mut room, index, text)?;

        self.store.commit(room)?;

        self.restored_uncertainty.remove(&key.attempt);

        Ok(Some(id))
    }

    pub fn fail_attempt(
        &mut self,
        key: AttemptEventKey,
        uncertain: bool,
    ) -> Result<bool, TeamError> {
        let Some(index) = event_attempt(self.store.room(), key) else {
            return Ok(false);
        };

        if !in_flight(&self.store.room().attempts[index].state) {
            return Ok(false);
        }

        let mut room = self.store.room().clone();

        fail(&mut room, index, uncertain)?;

        self.store.commit(room)?;

        if !uncertain {
            self.restored_uncertainty.remove(&key.attempt);
        }

        Ok(true)
    }

    pub fn saved_rooms(data_directory: &Path) -> Result<Vec<RoomId>, TeamError> {
        Ok(RoomStore::saved_rooms(data_directory)?)
    }

    pub fn add_member(&mut self, config: MemberConfig) -> Result<MemberId, TeamError> {
        let mut room = self.store.room().clone();

        let id = room.add_member(config)?;

        self.store.commit(room)?;

        Ok(id)
    }

    pub fn set_member_settings(
        &mut self,
        id: MemberId,
        settings: ThreadSettings,
    ) -> Result<(), TeamError> {
        let mut room = self.store.room().clone();

        room.set_member_settings(id, settings)?;

        self.store.commit(room)?;

        Ok(())
    }

    pub fn record_provider_identity(
        &mut self,
        id: MemberId,
        provider_id: &str,
        moderator_registered: bool,
    ) -> Result<(), TeamError> {
        let mut room = self.store.room().clone();

        let member = room
            .members
            .iter_mut()
            .find(|member| member.id == id)
            .ok_or(TeamError::Unavailable)?;

        if provider_id.trim().is_empty()
            || member
                .provider_id
                .as_deref()
                .is_some_and(|current| current != provider_id)
        {
            return Err(TeamError::Unavailable);
        }

        if member.provider_id.as_deref() == Some(provider_id)
            && member.moderator_registered == moderator_registered
        {
            return Ok(());
        }

        member.provider_id = Some(provider_id.to_owned());
        member.moderator_registered = moderator_registered;

        self.store.commit(room)?;

        Ok(())
    }

    pub fn exclude_member(&mut self, id: MemberId) -> Result<(), TeamError> {
        if self.has_live_attempts() || !self.restored_uncertainty.is_empty() {
            return Err(TeamError::Busy);
        }

        let mut room = self.store.room().clone();

        room.exclude_member(id)?;

        for discussion in &mut room.discussions {
            if discussion.participants.contains(&id) || discussion.mode.report_author() == id {
                discussion.pause(PauseReason::MemberUnavailable(id));
            }
        }

        self.store.commit(room)?;

        self.readiness.remove(&id);

        Ok(())
    }

    pub fn close(&mut self) -> Result<(), TeamError> {
        let mut room = self.store.room().clone();

        for discussion in &mut room.discussions {
            discussion.pause(PauseReason::Closed);
        }

        self.store.commit(room)?;

        Ok(())
    }

    pub fn set_automatic_summaries(&mut self, enabled: bool) -> Result<(), TeamError> {
        let mut room = self.store.room().clone();

        room.controls.automatic_summaries = enabled;

        if !enabled {
            for attempt in &mut room.attempts {
                if attempt.state != AttemptState::Reserved
                    || !matches!(attempt.intent.invocation, Invocation::PublicSummary(_))
                {
                    continue;
                }

                attempt.state = AttemptState::Rejected;
            }
        }

        self.store.commit(room)?;

        Ok(())
    }

    pub fn pending_recovery(&self) -> impl Iterator<Item = &Attempt> {
        self.store
            .room()
            .attempts()
            .iter()
            .filter(|attempt| self.restored_uncertainty.contains(&attempt.id))
    }

    /// The user can end tracking restored work without claiming that the
    /// provider never ran it. Its consumed budget and provider identity remain.
    pub fn abandon_restored_attempt(&mut self, id: AttemptId) -> Result<(), TeamError> {
        if self.has_live_attempts() || !self.restored_uncertainty.contains(&id) {
            return Err(TeamError::Unresolved);
        }

        let mut room = self.store.room().clone();

        abandon(&mut room, id)?;

        self.store.commit(room)?;

        self.restored_uncertainty.remove(&id);

        Ok(())
    }

    /// Adapters call this only for the registered control operation. Public
    /// replies are never decoded as scheduling requests, regardless of syntax.
    pub fn moderator_decision(
        &mut self,
        key: AttemptEventKey,
        stage_id: StageId,
        operation: OperationId,
        action: ModeratorAction,
    ) -> Result<bool, TeamError> {
        let Some(attempt) = self
            .store
            .room()
            .attempts
            .iter()
            .find(|attempt| attempt.id == key.attempt)
        else {
            return Ok(false);
        };

        let BudgetScope::Discussion(discussion_id) = attempt.intent.budget else {
            return Ok(false);
        };

        let discussion = self
            .store
            .room()
            .discussion(discussion_id)
            .ok_or(TeamError::Unavailable)?;

        let Some(stage) = discussion.stages.last() else {
            return Ok(false);
        };

        if stage.decision.is_some() {
            return Ok(false);
        }

        let valid = stage.id == stage_id
            && stage.kind == StageKind::ModeratorDecision
            && discussion.mode
                == (DiscussionMode::Moderated {
                    moderator: key.member,
                })
            && attempt.intent.purpose == TurnPurpose::Moderation
            && attempt.intent.operation == operation
            && attempt.intent.stage == Some(stage_id)
            && attempt.intent.recipient == key.member
            && attempt.intent.backend_generation == key.backend_generation
            && matches!(attempt.state, AttemptState::Accepted { .. })
            && self.store.room().member(key.member).is_some();

        if !valid {
            self.pause_discussion(discussion_id, PauseReason::InvalidModeration(operation))?;

            return Ok(false);
        }

        self.validate_mode(discussion.mode)?;

        self.validate_member(key.member)?;

        let purposes = match &action {
            ModeratorAction::Invite { recipients } => {
                let unique: BTreeSet<_> = recipients.iter().copied().collect();

                if recipients.is_empty()
                    || recipients.len() != unique.len()
                    || recipients.iter().any(|member| {
                        !discussion.participants.contains(member)
                            || self.validate_member(*member).is_err()
                    })
                {
                    self.pause_discussion(
                        discussion_id,
                        PauseReason::InvalidModeration(operation),
                    )?;

                    return Ok(false);
                }

                recipients
                    .iter()
                    .map(|_| TurnPurpose::Response)
                    .collect::<Vec<_>>()
            }
            ModeratorAction::Report => vec![TurnPurpose::Report],
        };

        if let Err(error) = discussion.budget.check_batch(
            self.store
                .room()
                .budget_attempts(BudgetScope::Discussion(discussion_id)),
            purposes.into_iter(),
        ) {
            self.pause_discussion(discussion_id, PauseReason::Budget)?;

            return Err(error.into());
        }

        let mut room = self.store.room().clone();

        let stage = room
            .discussion_mut(discussion_id)
            .ok_or(TeamError::Unavailable)?
            .stages
            .last_mut()
            .ok_or(TeamError::Unavailable)?;

        stage.decision = Some(ModeratorDecision {
            operation,
            attempt: key.attempt,
            actor: key.member,
            action,
        });

        self.store.commit(room)?;

        Ok(true)
    }
}

fn cancel_pending_reservations(room: &mut Room, id: DiscussionId) -> Result<(), TeamError> {
    room.discussion(id).ok_or(TeamError::Unavailable)?;

    for attempt in &mut room.attempts {
        if attempt.intent.budget == BudgetScope::Discussion(id)
            && attempt.state == AttemptState::Reserved
        {
            attempt.state = AttemptState::Rejected;
        }
    }

    Ok(())
}
