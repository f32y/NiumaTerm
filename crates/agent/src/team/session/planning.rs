use std::collections::BTreeSet;

use serde_json::json;

use crate::team::attempt::{AttemptState, BudgetScope, DispatchIntent, Invocation};
use crate::team::budget::TurnPurpose;
use crate::team::content::{Author, PublicMessage, Publication, UserInput};
use crate::team::context::{ContextError, ContextLimits};
use crate::team::discussion::{
    Arrangement, ArrangementState, DiscussionMode, DiscussionState, PauseReason, PublicSnapshot,
    Stage, StageKind,
};
use crate::team::identity::{AttemptId, DiscussionId, MemberId, MessageId, OperationId, StageId};
use crate::team::moderation::ModeratorAction;
use crate::team::session::{SummaryRequest, TeamError, TeamSession};
use crate::team::storage::DispatchError;

struct DispatchPlan {
    recipient: MemberId,
    operation: OperationId,
    stage: Option<(StageId, StageKind)>,
    budget: BudgetScope,
    purpose: TurnPurpose,
}

impl TeamSession {
    pub fn start_discussion(
        &mut self,
        input: UserInput,
        participants: Vec<MemberId>,
        mode: DiscussionMode,
    ) -> Result<DiscussionId, TeamError> {
        self.validate_input(&input)?;
        self.validate_mode(mode)?;

        let mut room = self.room().clone();

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

        let mut room = self.room().clone();

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

        let snapshot = self.room().public_snapshot();
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
        let mut room = self.room().clone();
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

            let (kind, recipients) = if discussion.budget.remaining_non_report_turns() == 0
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
                                        id,
                                        PauseReason::InvalidModeration(operation),
                                    )?;

                                    return Err(TeamError::Paused);
                                }
                            },

                            None => (StageKind::ModeratorDecision, vec![moderator]),
                        }
                    }
                }
            };

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
            .room()
            .discussions
            .iter()
            .find(|run| run.id == id)
            .ok_or(TeamError::Unavailable)?;

        let stage = discussion.stages.last().ok_or(TeamError::Unavailable)?;

        let preparations: Vec<_> = self
            .room()
            .attempts
            .iter()
            .filter(|attempt| {
                attempt.intent.stage == Some(stage.id)
                    && matches!(attempt.intent.invocation, Invocation::PublicSummary(_))
            })
            .collect();

        let queued: Vec<_> = preparations
            .iter()
            .filter(|attempt| attempt.state == AttemptState::Reserved)
            .map(|attempt| attempt.id)
            .collect();

        if !queued.is_empty() {
            return Ok(queued);
        }

        if preparations.iter().any(|attempt| {
            matches!(
                attempt.state,
                AttemptState::Sending | AttemptState::Accepted { .. } | AttemptState::Uncertain
            )
        }) {
            return Ok(Vec::new());
        }

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

            if let Some(attempt) = self.room().attempts.iter().find(|attempt| {
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

                Err(TeamError::Context(ContextError::NeedsSummaries(chunks))) => {
                    let request = SummaryRequest {
                        owner: discussion.mode.report_author(),
                        budget: BudgetScope::Discussion(id),
                        stage: Some(stage.id),
                        snapshot: snapshot.clone(),
                        chunks,
                    };

                    return match self.reserve_summaries(request, limits) {
                        Ok(ids) => Ok(ids),

                        Err(error) => {
                            self.pause_dispatch_error(id, &error)?;

                            Err(error)
                        }
                    };
                }

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

    pub(in crate::team) fn validate_mode(&self, mode: DiscussionMode) -> Result<(), TeamError> {
        let member = self
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
                .moderation
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
                .room()
                .discussions()
                .iter()
                .find(|discussion| discussion.id() == id)
                .ok_or(TeamError::Unavailable)?;

            let members: Vec<_> = discussion
                .participants()
                .iter()
                .filter_map(|id| self.room().member(*id))
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

        let prepared = self
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
        if input
            .references
            .iter()
            .any(|id| !self.room().messages.iter().any(|message| message.id == *id))
        {
            return Err(TeamError::Unavailable);
        }

        for attachment in &input.attachments {
            self.store.read_attachment(attachment)?;
        }

        Ok(())
    }
}

fn public_request(input: UserInput) -> PublicMessage {
    PublicMessage {
        id: MessageId::new(),
        author: Author::User,
        publication: Publication::UserInput,
        text: input.text,
        replies_to: input.references,
        attachments: input.attachments,
    }
}
