//! Durable room operations and live dispatch readiness owned by one Team.

pub use crate::team::session::dispatch::DispatchError;

pub use crate::team::session::outcomes::AttemptEventKey;

pub(super) mod attachments;

pub(super) mod dispatch;

mod controls;

mod outcomes;

mod planning;

#[cfg(test)]
mod planning_tests;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use std::fs;

use std::path::Path;

use serde_json::json;

use thiserror::Error;

use crate::chat::{SendOutcome, ThreadSettings};

use crate::session::team_capabilities::ModeratorAdmission;

use crate::team::attempt::{Attempt, AttemptState, BudgetScope, DispatchIntent, Invocation};

use crate::team::budget::{BudgetError, TurnPurpose};

use crate::team::content::{AttachmentReference, Author, PublicMessage, Publication, UserInput};

use crate::team::context::{ContextError, ContextLimits};

use crate::team::discussion::{
    Arrangement, ArrangementState, Discussion, DiscussionError, DiscussionMode, DiscussionState,
    PauseReason, PublicSnapshot, Stage, StageKind,
};

use crate::team::execution_slots::{ExecutionKey, ExecutionSlots, WorkStatus};

use crate::team::identity::{
    AttemptId, DiscussionId, MemberId, MessageId, OperationId, OwnershipGeneration, RoomId, StageId,
};

use crate::team::member::MemberConfig;

use crate::team::moderation::{ModeratorAction, ModeratorDecision};

use crate::team::room::{MemberError, Room};

use crate::team::session::attachments::read_attachment;

use crate::team::session::controls::cancel_pending_reservations;

use crate::team::session::planning::{DispatchPlan, public_request};

use crate::team::storage::{RoomStore, StorageError};

pub struct TeamSession {
    store: RoomStore,
    readiness: BTreeMap<MemberId, MemberReadiness>,
    slots: ExecutionSlots,
    restored_uncertainty: BTreeSet<AttemptId>,
}

struct MemberReadiness {
    ownership: OwnershipGeneration,
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
            slots: ExecutionSlots::default(),
            restored_uncertainty: BTreeSet::new(),
        })
    }

    pub fn open(data_directory: &Path, id: RoomId) -> Result<(Self, bool), TeamError> {
        let (mut store, truncated) = RoomStore::open(data_directory, id)?;
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
            .filter(|attempt| {
                matches!(
                    attempt.state,
                    AttemptState::Sending | AttemptState::Accepted { .. } | AttemptState::Uncertain
                )
            })
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

        Ok((
            Self {
                store,
                readiness: BTreeMap::new(),
                slots: ExecutionSlots::default(),
                restored_uncertainty,
            },
            truncated,
        ))
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

        let ownership = member.ownership();
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
                ownership,
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

            for attachment in intent.input.attachments.iter().chain(&intent.attachments) {
                self.read_attachment(attachment)?;
            }
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
                .discussions()
                .iter()
                .find(|run| run.id() == id)
                .ok_or(TeamError::Unavailable)?;

            if !matches!(
                run.state(),
                DiscussionState::Running | DiscussionState::Finishing
            ) || !run.pauses().is_empty()
            {
                return Err(TeamError::Paused);
            }
        }

        let key = ExecutionKey {
            member: attempt.intent.recipient,
            ownership: attempt.intent.ownership,
            attempt: id,
        };

        if !self.slots.reserve(key) {
            return Err(TeamError::Busy);
        }

        let mut sent = false;

        let result = dispatch::dispatch(&mut self.store, id, |intent| {
            sent = true;

            send(intent)
        });

        match &result {
            Ok(SendOutcome::NotReady) => {
                self.slots.update(key, WorkStatus::default());
            }

            Ok(SendOutcome::StartedTurn) => {}

            Ok(SendOutcome::Steered | SendOutcome::Rejected { .. }) => {
                self.slots.update(
                    key,
                    WorkStatus {
                        uncertain: true,
                        ..WorkStatus::default()
                    },
                );
            }

            Err(_) if !sent => {
                self.slots.update(key, WorkStatus::default());
            }

            Err(_) => {
                self.slots.update(
                    key,
                    WorkStatus {
                        uncertain: true,
                        ..WorkStatus::default()
                    },
                );
            }
        }

        Ok(result?)
    }

    pub fn update_work(&mut self, key: ExecutionKey, status: WorkStatus) -> bool {
        self.slots.update(key, status)
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

        if readiness.ownership != intent.ownership
            || readiness.backend_generation != intent.backend_generation
        {
            return Err(TeamError::Unavailable);
        }

        self.validate_member(intent.recipient)
    }

    pub(super) fn validate_member(&self, id: MemberId) -> Result<(), TeamError> {
        if !self.restored_uncertainty.is_empty() {
            return Err(TeamError::Unavailable);
        }

        let member = self.store.room().member(id).ok_or(TeamError::Unavailable)?;
        let readiness = self.readiness.get(&id).ok_or(TeamError::Unavailable)?;

        if member.excluded() || member.ownership() != readiness.ownership {
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

        let discussion = room
            .discussions
            .iter_mut()
            .find(|run| run.id == id)
            .ok_or(TeamError::Unavailable)?;

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

        let discussion = room
            .discussions
            .iter_mut()
            .find(|run| run.id == id)
            .ok_or(TeamError::Unavailable)?;

        if discussion.state == DiscussionState::Completed {
            return Ok(Vec::new());
        }

        if !discussion.pauses.is_empty()
            || !matches!(
                discussion.state,
                DiscussionState::Running | DiscussionState::Finishing
            )
        {
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
            if !self.slots.is_idle() {
                return Ok(Vec::new());
            }

            let (kind, recipients) = self.next_stage(discussion)?;

            discussion.stages.push(Stage {
                decision: None,
                id: StageId::new(),
                kind,
                arrangements: recipients
                    .into_iter()
                    .map(|recipient| Arrangement {
                        operation: OperationId::new(),
                        recipient,
                        state: ArrangementState::Pending,
                    })
                    .collect(),
                segments: vec![snapshot],
            });

            self.store.commit(room)?;
        }

        let discussion = self
            .store
            .room()
            .discussions
            .iter()
            .find(|run| run.id == id)
            .ok_or(TeamError::Unavailable)?;

        let stage = discussion.stages.last().ok_or(TeamError::Unavailable)?;

        let snapshot = stage.segments.last().ok_or(TeamError::Unavailable)?;

        let purpose = match stage.kind {
            StageKind::Report => TurnPurpose::Report,
            StageKind::ModeratorDecision => TurnPurpose::Moderation,
            _ => TurnPurpose::Response,
        };

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

    fn next_stage(
        &mut self,
        discussion: &Discussion,
    ) -> Result<(StageKind, Vec<MemberId>), TeamError> {
        Ok(
            if discussion.budget.remaining_non_report_turns() == 0
                && discussion
                    .stages
                    .last()
                    .is_none_or(|stage| stage.kind != StageKind::ModeratorDecision)
            {
                (StageKind::Report, vec![discussion.mode.report_author()])
            } else {
                match discussion.mode {
                    DiscussionMode::Fixed { report_author } => {
                        let kind = if !discussion
                            .stages
                            .iter()
                            .any(|stage| stage.kind == StageKind::InitialAnswers)
                        {
                            StageKind::InitialAnswers
                        } else if !discussion
                            .stages
                            .iter()
                            .any(|stage| stage.kind == StageKind::PeerResponses)
                        {
                            StageKind::PeerResponses
                        } else {
                            StageKind::Report
                        };

                        let recipients = if kind == StageKind::Report {
                            vec![report_author]
                        } else {
                            discussion.participants.clone()
                        };

                        (kind, recipients)
                    }

                    DiscussionMode::Moderated { moderator } => {
                        self.moderated_next_stage(discussion, moderator)?
                    }
                }
            },
        )
    }

    fn moderated_next_stage(
        &mut self,
        discussion: &Discussion,
        moderator: MemberId,
    ) -> Result<(StageKind, Vec<MemberId>), TeamError> {
        Ok(
            match discussion.stages.last().filter(|stage| {
                stage.kind == StageKind::ModeratorDecision
                    && stage.arrangements.iter().any(|entry| {
                        entry.recipient == moderator
                            && matches!(entry.state, ArrangementState::Completed(_))
                    })
            }) {
                Some(stage) => match &stage.decision {
                    Some(decision) => match &decision.action {
                        ModeratorAction::Invite { recipients } => {
                            (StageKind::InvitedResponses, recipients.clone())
                        }

                        ModeratorAction::Report => (StageKind::Report, vec![moderator]),
                    },

                    None => {
                        let operation = stage
                            .arrangements
                            .first()
                            .ok_or(TeamError::Unavailable)?
                            .operation;

                        self.pause_discussion(
                            discussion.id,
                            PauseReason::InvalidModeration(operation),
                        )?;

                        return Err(TeamError::Paused);
                    }
                },

                None => (StageKind::ModeratorDecision, vec![moderator]),
            },
        )
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
        let DispatchPlan {
            recipient,
            operation,
            stage,
            budget,
            purpose,
        } = plan;

        let member = self
            .store
            .room()
            .member(recipient)
            .ok_or(TeamError::Unavailable)?;

        let readiness = self
            .readiness
            .get(&recipient)
            .ok_or(TeamError::Unavailable)?;

        let instruction = match stage.map(|(_, kind)| kind) {
            Some(StageKind::InitialAnswers) => {
                "Give your initial answer to the user's objective. Identify assumptions and reasons."
            }

            Some(StageKind::PeerResponses | StageKind::InvitedResponses) => {
                "Respond to the preceding public contributions. Explain agreement, disagreement, and any changed view."
            }

            Some(StageKind::Report) => {
                "Conclude with separate agreements, disagreements, each member's attributed reasons, and suggested next steps. Cite public message IDs. Do not present one member's preference as consensus."
            }

            Some(StageKind::ModeratorDecision) => {
                "Review public progress and call team_decide exactly once. Invite eligible member IDs or request a report with an empty recipients list. Prose alone cannot schedule work."
            }

            _ => {
                "Answer this one request. Further Team turns require a separate scheduling decision."
            }
        };

        let mut instructions = format!(
            "Team role: {}\n{instruction}\n",
            json!({"name": member.name(), "role": member.role()})
        );

        if purpose == TurnPurpose::Moderation
            && let BudgetScope::Discussion(id) = budget
        {
            let discussion = self
                .store
                .room()
                .discussions()
                .iter()
                .find(|discussion| discussion.id() == id)
                .ok_or(TeamError::Unavailable)?;

            let members: Vec<_> = discussion
                .participants()
                .iter()
                .filter_map(|id| self.store.room().member(*id))
                .map(|member| json!({"id": member.id(), "name": member.name()}))
                .collect();

            instructions.push_str(&format!("Application scheduling context: {}\n", json!({"operation": operation, "stage": stage.map(|(id, _)| id), "eligible_members": members, "remaining_non_report_turns": discussion.budget().remaining_non_report_turns()})));
        }

        let body_limits = ContextLimits {
            max_bytes: limits
                .max_bytes
                .checked_sub(instructions.len())
                .ok_or(ContextError::OversizedInput)?,
            recent_messages: limits.recent_messages,
        };

        let prepared =
            self.store
                .room()
                .prepare_context(recipient, snapshot, &input, &body_limits)?;

        instructions.push_str(&prepared.text);

        let intent = DispatchIntent {
            invocation: Invocation::MemberConversation,
            recipient,
            ownership: member.ownership,
            backend_generation: readiness.backend_generation,
            operation,
            stage: stage.map(|(id, _)| id),
            budget,
            purpose,
            input,
            attachments: prepared.attachments,
            prepared_text: instructions,
            snapshot: snapshot.clone(),
            coverage: prepared.coverage,
        };

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

        for attachment in &input.attachments {
            self.read_attachment(attachment)?;
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
            .discussions
            .iter_mut()
            .find(|run| run.id == id)
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
            .discussions
            .iter_mut()
            .find(|run| run.id == id)
            .ok_or(TeamError::Unavailable)?
            .resolve_pause(reason);

        self.store.commit(room)?;

        Ok(changed)
    }

    pub fn continue_discussion(&mut self, id: DiscussionId) -> Result<(), TeamError> {
        if !self.slots.is_idle() || !self.restored_uncertainty.is_empty() {
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

        let discussion = room
            .discussions
            .iter_mut()
            .find(|run| run.id == id)
            .ok_or(TeamError::Unavailable)?;

        discussion.budget.add_turns(turns)?;
        discussion.resolve_pause(&PauseReason::Budget);
        self.store.commit(room)?;

        Ok(())
    }

    pub fn change_mode(&mut self, id: DiscussionId, mode: DiscussionMode) -> Result<(), TeamError> {
        self.pause_discussion(id, PauseReason::ModeChange)?;

        if !self.slots.is_idle() {
            return Err(TeamError::Busy);
        }

        self.validate_mode(mode)?;

        let mut room = self.store.room().clone();

        cancel_pending_reservations(&mut room, id)?;

        let discussion = room
            .discussions
            .iter_mut()
            .find(|run| run.id == id)
            .ok_or(TeamError::Unavailable)?;

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
        if !self.slots.is_idle() || !self.restored_uncertainty.is_empty() {
            return Err(TeamError::Unresolved);
        }

        let mut room = self.store.room().clone();

        cancel_pending_reservations(&mut room, id)?;

        let discussion = room
            .discussions
            .iter_mut()
            .find(|run| run.id == id)
            .ok_or(TeamError::Unavailable)?;

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

        if !self.slots.is_idle() || !self.restored_uncertainty.is_empty() {
            return Err(TeamError::Unresolved);
        }

        let mut room = self.store.room().clone();

        cancel_pending_reservations(&mut room, id)?;

        let snapshot = room.public_snapshot();

        let discussion = room
            .discussions
            .iter_mut()
            .find(|run| run.id == id)
            .ok_or(TeamError::Unavailable)?;

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

        discussion.stages.push(Stage {
            decision: None,
            id: StageId::new(),
            kind: StageKind::Report,
            arrangements: vec![Arrangement {
                operation: OperationId::new(),
                recipient: discussion.mode.report_author(),
                state: ArrangementState::Pending,
            }],
            segments: vec![snapshot],
        });

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
        let Some(index) = self.event_attempt(key) else {
            return Ok(false);
        };

        let attempt = &self.store.room().attempts[index];

        if provider_turn.is_empty()
            || !matches!(
                attempt.state,
                AttemptState::Sending | AttemptState::Uncertain
            )
            || attempt
                .provider_turn
                .as_deref()
                .is_some_and(|id| id != provider_turn)
        {
            return Ok(false);
        }

        let mut room = self.store.room().clone();
        let attempt = &mut room.attempts[index];

        attempt.provider_turn = Some(provider_turn.to_owned());

        attempt.state = AttemptState::Accepted {
            provider_turn: provider_turn.to_owned(),
        };

        if attempt.intent.purpose != TurnPurpose::Summary {
            let member = room
                .members
                .iter_mut()
                .find(|member| member.id == key.member)
                .ok_or(TeamError::Unavailable)?;

            member
                .coverage
                .messages
                .extend(&attempt.intent.coverage.messages);

            member
                .coverage
                .summaries
                .extend(&attempt.intent.coverage.summaries);
        }

        self.store.commit(room)?;

        Ok(true)
    }

    pub fn complete_reply(
        &mut self,
        key: AttemptEventKey,
        provider_turn: &str,
        text: String,
        remaining_work: WorkStatus,
    ) -> Result<Option<MessageId>, TeamError> {
        let Some(index) = self.event_attempt(key) else {
            return Ok(None);
        };

        let attempt = &self.store.room().attempts[index];

        if !matches!(&attempt.state, AttemptState::Accepted { provider_turn: accepted } if accepted == provider_turn)
            || attempt.intent.purpose == TurnPurpose::Summary
        {
            return Ok(None);
        }

        let mut room = self.store.room().clone();

        let member = room
            .members
            .iter_mut()
            .find(|member| member.id == key.member)
            .ok_or(TeamError::Unavailable)?;

        let id = MessageId::new();

        let publication = match attempt.intent.purpose {
            TurnPurpose::Report => Publication::Report,
            TurnPurpose::Moderation => Publication::ModeratorDecision,
            _ => Publication::RootReply,
        };

        room.messages.push(PublicMessage {
            id,
            author: Author::Member {
                id: member.id,
                name: member.name.clone(),
            },
            publication,
            text,
            replies_to: attempt.intent.input.references.clone(),
            attachments: Vec::new(),
        });

        member.coverage.messages.insert(id);
        room.attempts[index].state = AttemptState::Completed { message: id };

        if let BudgetScope::Discussion(discussion_id) = attempt.intent.budget {
            let discussion = room
                .discussions
                .iter_mut()
                .find(|discussion| discussion.id == discussion_id)
                .ok_or(TeamError::Unavailable)?;

            for arrangement in discussion
                .stages
                .iter_mut()
                .flat_map(|stage| &mut stage.arrangements)
            {
                if arrangement.operation == attempt.intent.operation {
                    arrangement.state = ArrangementState::Completed(key.attempt);
                }
            }

            discussion.resolve_pause(&PauseReason::UncertainAttempt(key.attempt));

            if attempt.intent.purpose == TurnPurpose::Report {
                discussion.state = DiscussionState::Completed;
            } else if !discussion.pauses.is_empty() {
                discussion.settle_pause();
            }
        }

        self.store.commit(room)?;
        self.restored_uncertainty.remove(&key.attempt);

        self.slots.update(
            ExecutionKey {
                member: key.member,
                ownership: key.ownership,
                attempt: key.attempt,
            },
            remaining_work,
        );

        Ok(Some(id))
    }

    pub fn fail_attempt(
        &mut self,
        key: AttemptEventKey,
        uncertain: bool,
    ) -> Result<bool, TeamError> {
        let Some(index) = self.event_attempt(key) else {
            return Ok(false);
        };

        let attempt = &self.store.room().attempts[index];

        if !matches!(
            attempt.state,
            AttemptState::Sending | AttemptState::Accepted { .. } | AttemptState::Uncertain
        ) {
            return Ok(false);
        }

        let mut room = self.store.room().clone();

        room.attempts[index].state = if uncertain {
            AttemptState::Uncertain
        } else {
            AttemptState::Failed
        };

        if let BudgetScope::Discussion(id) = attempt.intent.budget {
            let discussion = room
                .discussions
                .iter_mut()
                .find(|run| run.id == id)
                .ok_or(TeamError::Unavailable)?;

            for arrangement in discussion
                .stages
                .iter_mut()
                .flat_map(|stage| &mut stage.arrangements)
            {
                if arrangement.operation == attempt.intent.operation {
                    arrangement.state = if uncertain {
                        ArrangementState::Uncertain(key.attempt)
                    } else {
                        ArrangementState::Failed(key.attempt)
                    };
                }
            }

            discussion.pause(if uncertain {
                PauseReason::UncertainAttempt(key.attempt)
            } else if attempt.intent.purpose == TurnPurpose::Summary {
                PauseReason::SummaryFailed(key.attempt)
            } else {
                PauseReason::AttemptFailed(key.attempt)
            });
        }

        self.store.commit(room)?;

        if !uncertain {
            self.restored_uncertainty.remove(&key.attempt);
        }

        // A failed response says nothing about surviving background commands.
        // Their confirmed status must arrive before the execution slot is free.
        Ok(true)
    }

    fn event_attempt(&self, key: AttemptEventKey) -> Option<usize> {
        let member = self.store.room().member(key.member)?;

        if member.ownership != key.ownership {
            return None;
        }

        self.store.room().attempts.iter().position(|attempt| {
            attempt.id == key.attempt
                && attempt.intent.recipient == key.member
                && attempt.intent.ownership == key.ownership
                && attempt.intent.backend_generation == key.backend_generation
        })
    }

    pub fn saved_rooms(data_directory: &Path) -> Result<Vec<RoomId>, TeamError> {
        let directory = data_directory.join("agent-teams");

        if !directory.exists() {
            return Ok(Vec::new());
        }

        let mut rooms = Vec::new();

        for entry in fs::read_dir(directory).map_err(StorageError::from)? {
            let entry = entry.map_err(StorageError::from)?;

            if entry.file_type().map_err(StorageError::from)?.is_dir()
                && let Some(id) = entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.parse().ok())
            {
                rooms.push(id);
            }
        }

        rooms.sort();

        Ok(rooms)
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
        ownership: OwnershipGeneration,
        settings: ThreadSettings,
    ) -> Result<(), TeamError> {
        let mut room = self.store.room().clone();

        room.set_member_settings(id, ownership, settings)?;
        self.store.commit(room)?;

        Ok(())
    }

    pub fn record_provider_identity(
        &mut self,
        id: MemberId,
        ownership: OwnershipGeneration,
        provider_id: &str,
        moderator_registered: bool,
    ) -> Result<(), TeamError> {
        let mut room = self.store.room().clone();

        let member = room
            .members
            .iter_mut()
            .find(|member| member.id == id && member.ownership == ownership)
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
        if !self.slots.is_idle() || !self.restored_uncertainty.is_empty() {
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

    pub fn read_attachment(&self, reference: &AttachmentReference) -> Result<Vec<u8>, TeamError> {
        Ok(read_attachment(self.store.directory(), reference)?)
    }

    pub fn close(&mut self) -> Result<(), TeamError> {
        let mut room = self.store.room().clone();

        for discussion in &mut room.discussions {
            discussion.pause(PauseReason::Closed);
        }

        self.store.commit(room)?;
        self.store.checkpoint()?;

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

                let budget = match attempt.intent.budget {
                    BudgetScope::Discussion(id) => room
                        .discussions
                        .iter_mut()
                        .find(|run| run.id == id)
                        .map(|run| &mut run.budget),

                    BudgetScope::Direct(id) => room.direct_allowances.get_mut(&id),
                }
                .ok_or(TeamError::Unavailable)?;

                budget.cancel_unsent(attempt.id)?;
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
        if !self.slots.is_idle() || !self.restored_uncertainty.contains(&id) {
            return Err(TeamError::Unresolved);
        }

        let mut room = self.store.room().clone();

        let attempt = room
            .attempts
            .iter_mut()
            .find(|attempt| attempt.id == id)
            .ok_or(TeamError::Unavailable)?;

        attempt.state = AttemptState::Abandoned;

        for discussion in &mut room.discussions {
            for arrangement in discussion
                .stages
                .iter_mut()
                .flat_map(|stage| &mut stage.arrangements)
            {
                if matches!(arrangement.state, ArrangementState::Active(attempt) | ArrangementState::Uncertain(attempt) if attempt == id)
                {
                    arrangement.state = ArrangementState::Skipped;
                }
            }

            discussion.resolve_pause(&PauseReason::UncertainAttempt(id));
            discussion.pause(PauseReason::User);
            discussion.settle_pause();
        }

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
            .discussions
            .iter()
            .find(|run| run.id == discussion_id)
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
            && attempt.intent.ownership == key.ownership
            && attempt.intent.backend_generation == key.backend_generation
            && matches!(attempt.state, AttemptState::Accepted { .. })
            && self
                .store
                .room()
                .member(key.member)
                .is_some_and(|member| member.ownership == key.ownership);

        if !valid {
            self.pause_discussion(discussion_id, PauseReason::InvalidModeration(operation))?;

            return Ok(false);
        }

        self.validate_mode(discussion.mode)?;
        self.validate_member(key.member)?;

        let reservations = match &action {
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
                    .map(|_| (AttemptId::new(), TurnPurpose::Response))
                    .collect::<Vec<_>>()
            }

            ModeratorAction::Report => vec![(AttemptId::new(), TurnPurpose::Report)],
        };

        let mut budget = discussion.budget.clone();

        if let Err(error) = budget.reserve(&reservations) {
            self.pause_discussion(discussion_id, PauseReason::Budget)?;

            return Err(error.into());
        }

        let mut room = self.store.room().clone();

        let discussion = room
            .discussions
            .iter_mut()
            .find(|run| run.id == discussion_id)
            .ok_or(TeamError::Unavailable)?;

        let stage = discussion.stages.last_mut().ok_or(TeamError::Unavailable)?;

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
