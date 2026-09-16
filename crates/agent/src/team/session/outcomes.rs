//! How a provider's report about one sent attempt changes the room.

use crate::team::attempt::{Attempt, AttemptState, BudgetScope};
use crate::team::budget::TurnPurpose;
use crate::team::discussion::{ArrangementState, DiscussionState, PauseReason};
use crate::team::model::{
    AttemptId, Author, MemberId, MessageId, OwnershipGeneration, PublicMessage, Publication,
};
use crate::team::room::Room;
use crate::team::session::TeamError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttemptEventKey {
    pub attempt: AttemptId,
    pub member: MemberId,
    pub ownership: OwnershipGeneration,
    pub backend_generation: u64,
}

/// The index of the attempt `key` names, when its member still holds the
/// ownership and backend generation the event was raised under.
pub(super) fn event_attempt(room: &Room, key: AttemptEventKey) -> Option<usize> {
    let member = room.member(key.member)?;

    if member.ownership != key.ownership {
        return None;
    }

    room.attempts.iter().position(|attempt| {
        attempt.id == key.attempt
            && attempt.intent.recipient == key.member
            && attempt.intent.ownership == key.ownership
            && attempt.intent.backend_generation == key.backend_generation
    })
}

/// Whether an attempt was sent and has not reached an outcome yet.
pub(super) fn in_flight(state: &AttemptState) -> bool {
    matches!(
        state,
        AttemptState::Sending | AttemptState::Accepted { .. } | AttemptState::Uncertain
    )
}

/// Whether `attempt` can take `provider_turn` as its acceptance: it is still
/// waiting for one, and any turn it already recorded is the same turn.
pub(super) fn accepts(attempt: &Attempt, provider_turn: &str) -> bool {
    !provider_turn.is_empty()
        && matches!(
            attempt.state,
            AttemptState::Sending | AttemptState::Uncertain
        )
        && attempt
            .provider_turn
            .as_deref()
            .is_none_or(|id| id == provider_turn)
}

/// Record that the provider accepted attempt `index` as `provider_turn`. Apart
/// from a summary turn, the recipient's accepted coverage grows by the context
/// the attempt delivered.
pub(super) fn accept(room: &mut Room, index: usize, provider_turn: &str) -> Result<(), TeamError> {
    let attempt = &mut room.attempts[index];

    attempt.provider_turn = Some(provider_turn.to_owned());

    attempt.state = AttemptState::Accepted {
        provider_turn: provider_turn.to_owned(),
    };

    if attempt.intent.purpose != TurnPurpose::Summary {
        let recipient = attempt.intent.recipient;

        let member = room
            .members
            .iter_mut()
            .find(|member| member.id == recipient)
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

    Ok(())
}

/// Whether `attempt` can be completed by a reply to `provider_turn`: that is
/// the turn it was accepted as, and it is a turn that publishes a reply.
pub(super) fn completes(attempt: &Attempt, provider_turn: &str) -> bool {
    matches!(&attempt.state, AttemptState::Accepted { provider_turn: accepted } if accepted == provider_turn)
        && attempt.intent.purpose != TurnPurpose::Summary
}

/// Publish `text` as the reply attempt `index` produced and settle the
/// arrangement it served. A report ends its discussion.
pub(super) fn complete(
    room: &mut Room,
    index: usize,
    text: String,
) -> Result<MessageId, TeamError> {
    let attempt = room.attempts[index].id;
    let intent = &room.attempts[index].intent;

    let (recipient, budget, operation, purpose) = (
        intent.recipient,
        intent.budget,
        intent.operation,
        intent.purpose,
    );

    let replies_to = intent.input.references.clone();

    let member = room
        .members
        .iter_mut()
        .find(|member| member.id == recipient)
        .ok_or(TeamError::Unavailable)?;

    let id = MessageId::new();

    let publication = match purpose {
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
        replies_to,
        attachments: Vec::new(),
    });

    member.coverage.messages.insert(id);

    room.attempts[index].state = AttemptState::Completed { message: id };

    if let BudgetScope::Discussion(discussion_id) = budget {
        let discussion = room
            .discussion_mut(discussion_id)
            .ok_or(TeamError::Unavailable)?;

        discussion.mark_operation(operation, ArrangementState::Completed(attempt));

        discussion.resolve_pause(&PauseReason::UncertainAttempt(attempt));

        if purpose == TurnPurpose::Report {
            discussion.state = DiscussionState::Completed;
        } else if !discussion.pauses.is_empty() {
            discussion.settle_pause();
        }
    }

    Ok(id)
}

/// Record that attempt `index` failed, or that its outcome is `uncertain`,
/// and pause its discussion on it.
pub(super) fn fail(room: &mut Room, index: usize, uncertain: bool) -> Result<(), TeamError> {
    let attempt = &mut room.attempts[index];

    attempt.state = if uncertain {
        AttemptState::Uncertain
    } else {
        AttemptState::Failed
    };

    let (id, budget, operation, purpose) = (
        attempt.id,
        attempt.intent.budget,
        attempt.intent.operation,
        attempt.intent.purpose,
    );

    if let BudgetScope::Discussion(discussion_id) = budget {
        let discussion = room
            .discussion_mut(discussion_id)
            .ok_or(TeamError::Unavailable)?;

        if uncertain {
            discussion.mark_operation(operation, ArrangementState::Uncertain(id));

            discussion.pause(PauseReason::UncertainAttempt(id));
        } else {
            discussion.mark_operation(operation, ArrangementState::Failed(id));

            discussion.pause(PauseReason::attempt_failed(id, purpose));
        }
    }

    Ok(())
}

/// Stop tracking restored attempt `id`. The arrangements it held are skipped
/// and every discussion waits for the user to continue.
pub(super) fn abandon(room: &mut Room, id: AttemptId) -> Result<(), TeamError> {
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

    Ok(())
}
