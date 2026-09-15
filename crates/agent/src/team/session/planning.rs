use crate::team::attempt::BudgetScope;
use crate::team::budget::TurnPurpose;
use crate::team::content::{Author, PublicMessage, Publication, UserInput};
use crate::team::discussion::{ArrangementState, Discussion, DiscussionMode, StageKind};
use crate::team::identity::{MemberId, MessageId, OperationId, StageId};
use crate::team::moderation::ModeratorAction;
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
