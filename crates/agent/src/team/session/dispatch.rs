use std::iter;

use thiserror::Error;

use crate::chat::SendOutcome;
use crate::team::attempt::{Attempt, AttemptState, BudgetScope, DispatchIntent};
use crate::team::budget::{Budget, BudgetError, TurnPurpose};
use crate::team::discussion::{ArrangementState, PauseReason};
use crate::team::model::AttemptId;
use crate::team::storage::{RoomStore, StorageError};

#[derive(Debug, Error)]
pub enum DispatchError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Budget(#[from] BudgetError),
    #[error("attempt is unavailable, already sent, or missing its recipient")]
    Ineligible,
}

pub(crate) fn reserve_dispatches(
    store: &mut RoomStore,
    intents: Vec<DispatchIntent>,
) -> Result<Vec<AttemptId>, DispatchError> {
    let mut next = store.room().clone();
    let mut ids = Vec::with_capacity(intents.len());

    for intent in intents {
        next.member(intent.recipient)
            .ok_or(DispatchError::Ineligible)?;

        let id = AttemptId::new();

        let direct_budget = Budget::direct();

        let budget = match intent.budget {
            BudgetScope::Discussion(discussion) => {
                &next
                    .discussion(discussion)
                    .ok_or(DispatchError::Ineligible)?
                    .budget
            }
            BudgetScope::Direct(_) => &direct_budget,
        };

        budget.check_batch(
            next.budget_attempts(intent.budget),
            iter::once(intent.purpose),
        )?;

        ids.push(id);

        next.attempts.push(Attempt {
            id,
            intent,
            state: AttemptState::Reserved,
            provider_turn: None,
        });
    }

    store.commit(next)?;

    Ok(ids)
}

/// The caller supplies a ready, idle session. Persistence precedes every
/// external send, including the durable change from reserved to charged.
/// A transport-level start is still awaiting provider acceptance evidence.
pub(crate) fn dispatch(
    store: &mut RoomStore,
    id: AttemptId,
    send: impl FnOnce(&DispatchIntent) -> SendOutcome,
) -> Result<SendOutcome, DispatchError> {
    let mut next = store.room().clone();

    let index = next
        .attempts
        .iter()
        .position(|attempt| attempt.id == id)
        .ok_or(DispatchError::Ineligible)?;

    let attempt = &next.attempts[index];

    if attempt.state != AttemptState::Reserved || next.member(attempt.intent.recipient).is_none() {
        return Err(DispatchError::Ineligible);
    }

    let intent = attempt.intent.clone();

    next.attempts[index].state = AttemptState::Sending;

    if let BudgetScope::Discussion(discussion_id) = intent.budget
        && intent.purpose != TurnPurpose::Summary
    {
        let discussion = next
            .discussion_mut(discussion_id)
            .ok_or(DispatchError::Ineligible)?;

        let arrangement = discussion
            .stages
            .iter_mut()
            .flat_map(|stage| &mut stage.arrangements)
            .find(|entry| entry.operation == intent.operation)
            .ok_or(DispatchError::Ineligible)?;

        if arrangement.state != ArrangementState::Pending {
            return Err(DispatchError::Ineligible);
        }

        arrangement.state = ArrangementState::Active(id);
    }

    store.commit(next)?;

    let outcome = send(&intent);

    match &outcome {
        SendOutcome::StartedTurn => {}
        SendOutcome::Steered | SendOutcome::Rejected { .. } => {
            let mut next = store.room().clone();

            next.attempts[index].state = AttemptState::Uncertain;

            if let BudgetScope::Discussion(discussion_id) = intent.budget {
                let discussion = next
                    .discussion_mut(discussion_id)
                    .ok_or(DispatchError::Ineligible)?;

                discussion.mark_operation(intent.operation, ArrangementState::Uncertain(id));

                discussion.pause(PauseReason::UncertainAttempt(id));
            }

            store.commit(next)?;
        }
        SendOutcome::NotReady => {
            let mut next = store.room().clone();

            next.attempts[index].state = AttemptState::Rejected;

            if let BudgetScope::Discussion(discussion_id) = intent.budget {
                let discussion = next
                    .discussion_mut(discussion_id)
                    .ok_or(DispatchError::Ineligible)?;

                discussion.mark_operation(intent.operation, ArrangementState::Failed(id));

                discussion.pause(PauseReason::attempt_failed(id, intent.purpose));
            }

            store.commit(next)?;
        }
    }

    Ok(outcome)
}
