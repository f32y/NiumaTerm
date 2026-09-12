use thiserror::Error;

use crate::chat::SendOutcome;
use crate::team::attempt::{Attempt, AttemptState, BudgetScope, DispatchIntent};
use crate::team::budget::{Budget, BudgetError, TurnPurpose};
use crate::team::discussion::{ArrangementState, PauseReason};
use crate::team::identity::AttemptId;
use crate::team::room::Room;
use crate::team::storage::{RoomStore, StorageError};

#[derive(Debug, Error)]
pub enum DispatchError {
    #[error(transparent)]
    Storage(#[from] StorageError),

    #[error(transparent)]
    Budget(#[from] BudgetError),

    #[error("attempt is unavailable, already sent, or belongs to an older owner")]
    Ineligible,
}

impl RoomStore {
    pub fn reserve_dispatches(
        &mut self,
        intents: Vec<DispatchIntent>,
    ) -> Result<Vec<AttemptId>, DispatchError> {
        let mut next = self.room.clone();
        let mut ids = Vec::with_capacity(intents.len());

        for intent in intents {
            let member = next
                .member(intent.recipient)
                .ok_or(DispatchError::Ineligible)?;

            if member.ownership() != intent.ownership {
                return Err(DispatchError::Ineligible);
            }

            let id = AttemptId::new();

            budget_mut(&mut next, intent.budget)?.reserve(&[(id, intent.purpose)])?;
            ids.push(id);

            next.attempts.push(Attempt {
                id,
                intent,
                state: AttemptState::Reserved,
                provider_turn: None,
            });
        }

        self.commit(next)?;

        Ok(ids)
    }

    /// The caller supplies a ready, idle session. Persistence precedes every
    /// external send, including the durable change from reserved to charged.
    /// A transport-level start is still awaiting provider acceptance evidence.
    pub(in crate::team) fn dispatch(
        &mut self,
        id: AttemptId,
        send: impl FnOnce(&DispatchIntent) -> SendOutcome,
    ) -> Result<SendOutcome, DispatchError> {
        let mut next = self.room.clone();

        let index = next
            .attempts
            .iter()
            .position(|attempt| attempt.id == id)
            .ok_or(DispatchError::Ineligible)?;

        let attempt = &next.attempts[index];

        if attempt.state != AttemptState::Reserved
            || next
                .member(attempt.intent.recipient)
                .is_none_or(|member| member.ownership() != attempt.intent.ownership)
        {
            return Err(DispatchError::Ineligible);
        }

        let intent = attempt.intent.clone();

        budget_mut(&mut next, intent.budget)?.charge(id)?;
        next.attempts[index].state = AttemptState::Sending;

        if let BudgetScope::Discussion(discussion_id) = intent.budget
            && intent.purpose != TurnPurpose::Summary
        {
            let discussion = next
                .discussions
                .iter_mut()
                .find(|run| run.id == discussion_id)
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

        self.commit(next)?;

        let outcome = send(&intent);

        match &outcome {
            SendOutcome::StartedTurn => {}

            SendOutcome::Steered | SendOutcome::Rejected { .. } => {
                let mut next = self.room.clone();

                next.attempts[index].state = AttemptState::Uncertain;

                if let BudgetScope::Discussion(discussion_id) = intent.budget {
                    let discussion = next
                        .discussions
                        .iter_mut()
                        .find(|run| run.id == discussion_id)
                        .ok_or(DispatchError::Ineligible)?;

                    for arrangement in discussion
                        .stages
                        .iter_mut()
                        .flat_map(|stage| &mut stage.arrangements)
                    {
                        if arrangement.operation == intent.operation {
                            arrangement.state = ArrangementState::Uncertain(id);
                        }
                    }

                    discussion.pause(PauseReason::UncertainAttempt(id));
                }

                self.commit(next)?;
            }

            SendOutcome::NotReady => {
                let mut next = self.room.clone();

                budget_mut(&mut next, intent.budget)?.release_rejected(id)?;
                next.attempts[index].state = AttemptState::Rejected;

                if let BudgetScope::Discussion(discussion_id) = intent.budget {
                    let discussion = next
                        .discussions
                        .iter_mut()
                        .find(|run| run.id == discussion_id)
                        .ok_or(DispatchError::Ineligible)?;

                    for arrangement in discussion
                        .stages
                        .iter_mut()
                        .flat_map(|stage| &mut stage.arrangements)
                    {
                        if arrangement.operation == intent.operation {
                            arrangement.state = ArrangementState::Failed(id);
                        }
                    }

                    discussion.pause(if intent.purpose == TurnPurpose::Summary {
                        PauseReason::SummaryFailed(id)
                    } else {
                        PauseReason::AttemptFailed(id)
                    });
                }

                self.commit(next)?;
            }
        }

        Ok(outcome)
    }
}

fn budget_mut(room: &mut Room, scope: BudgetScope) -> Result<&mut Budget, DispatchError> {
    match scope {
        BudgetScope::Discussion(id) => room
            .discussions
            .iter_mut()
            .find(|run| run.id == id)
            .map(|run| &mut run.budget)
            .ok_or(DispatchError::Ineligible),

        BudgetScope::Direct(id) => Ok(room
            .direct_allowances
            .entry(id)
            .or_insert_with(Budget::direct)),
    }
}
