use serde_json::json;

use crate::team::attempt::{BudgetScope, DispatchIntent, Invocation};
use crate::team::budget::TurnPurpose;
use crate::team::discussion::{
    ArrangementState, Discussion, DiscussionMode, ModeratorAction, PublicSnapshot, StageKind,
};
use crate::team::model::{
    Author, ContextError, ContextLimits, MemberId, MessageId, OperationId, PublicMessage,
    Publication, StageId, UserInput,
};
use crate::team::room::Room;
use crate::team::session::TeamError;

pub(super) struct DispatchPlan {
    pub(super) recipient: MemberId,
    pub(super) operation: OperationId,
    pub(super) stage: Option<(StageId, StageKind)>,
    pub(super) budget: BudgetScope,
    pub(super) purpose: TurnPurpose,
}

/// What a discussion whose last stage settled does next.
pub(super) enum NextStage {
    /// Open a stage of this kind for these recipients.
    Schedule(StageKind, Vec<MemberId>),
    /// The moderator's decision stage completed without recording a
    /// decision; the discussion pauses on this moderation operation.
    InvalidModeration(OperationId),
}

pub(super) fn public_request(input: UserInput) -> PublicMessage {
    PublicMessage {
        id: MessageId::new(),
        author: Author::User,
        publication: Publication::UserInput,
        text: input.text,
        replies_to: input.references,
        attachments: input.attachments,
    }
}

/// The stage `discussion` opens once its last one settled. A spent turn budget
/// goes straight to the report, unless the moderator was just asked to decide.
pub(super) fn next_stage(discussion: &Discussion) -> Result<NextStage, TeamError> {
    if discussion.budget.remaining_non_report_turns() == 0
        && discussion
            .stages
            .last()
            .is_none_or(|stage| stage.kind != StageKind::ModeratorDecision)
    {
        return Ok(NextStage::Schedule(
            StageKind::Report,
            vec![discussion.mode.report_author()],
        ));
    }

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

            Ok(NextStage::Schedule(kind, recipients))
        }
        DiscussionMode::Moderated { moderator } => moderated_next_stage(discussion, moderator),
    }
}

fn moderated_next_stage(
    discussion: &Discussion,
    moderator: MemberId,
) -> Result<NextStage, TeamError> {
    let decided = discussion.stages.last().filter(|stage| {
        stage.kind == StageKind::ModeratorDecision
            && stage.arrangements.iter().any(|entry| {
                entry.recipient == moderator
                    && matches!(entry.state, ArrangementState::Completed(_))
            })
    });

    let Some(stage) = decided else {
        return Ok(NextStage::Schedule(
            StageKind::ModeratorDecision,
            vec![moderator],
        ));
    };

    Ok(match &stage.decision {
        Some(decision) => match &decision.action {
            ModeratorAction::Invite { recipients } => {
                NextStage::Schedule(StageKind::InvitedResponses, recipients.clone())
            }
            ModeratorAction::Report => NextStage::Schedule(StageKind::Report, vec![moderator]),
        },
        None => NextStage::InvalidModeration(
            stage
                .arrangements
                .first()
                .ok_or(TeamError::Unavailable)?
                .operation,
        ),
    })
}

/// What a turn in a stage of `kind` is for.
pub(super) fn stage_purpose(kind: StageKind) -> TurnPurpose {
    match kind {
        StageKind::Report => TurnPurpose::Report,
        StageKind::ModeratorDecision => TurnPurpose::Moderation,
        _ => TurnPurpose::Response,
    }
}

// The text a member is sent for one turn of Team work.

/// Build the intent that sends `plan`'s recipient its turn: its role, what a
/// turn of this stage asks of it, the scheduling context a moderator decides
/// from, and the room context `snapshot` leaves it, within `limits`.
pub(super) fn build_intent(
    room: &Room,
    plan: DispatchPlan,
    backend_generation: u64,
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

    let member = room.member(recipient).ok_or(TeamError::Unavailable)?;

    let instruction = stage_instruction(stage.map(|(_, kind)| kind));

    let mut instructions = format!(
        "Team role: {}\n{instruction}\n",
        json!({"name": member.name(), "role": member.role()})
    );

    if purpose == TurnPurpose::Moderation
        && let BudgetScope::Discussion(id) = budget
    {
        let discussion = room.discussion(id).ok_or(TeamError::Unavailable)?;

        let members: Vec<_> = discussion
            .participants()
            .iter()
            .filter_map(|id| room.member(*id))
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

    let prepared = room.prepare_context(recipient, snapshot, &input, &body_limits)?;

    instructions.push_str(&prepared.text);

    Ok(DispatchIntent {
        invocation: Invocation::MemberConversation,
        recipient,
        ownership: member.ownership,
        backend_generation,
        operation,
        stage: stage.map(|(id, _)| id),
        budget,
        purpose,
        input,
        attachments: prepared.attachments,
        prepared_text: instructions,
        snapshot: snapshot.clone(),
        coverage: prepared.coverage,
    })
}

fn stage_instruction(kind: Option<StageKind>) -> &'static str {
    match kind {
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
        _ => "Answer this one request. Further Team turns require a separate scheduling decision.",
    }
}
