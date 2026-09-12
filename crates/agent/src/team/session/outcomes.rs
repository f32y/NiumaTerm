use crate::team::attempt::{AttemptState, BudgetScope};
use crate::team::budget::TurnPurpose;
use crate::team::content::{Author, PublicMessage, Publication};
use crate::team::discussion::{ArrangementState, DiscussionState, PauseReason};
use crate::team::execution_slots::{ExecutionKey, WorkStatus};
use crate::team::identity::{AttemptId, MemberId, MessageId, OwnershipGeneration};
use crate::team::session::{TeamError, TeamSession};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttemptEventKey {
    pub attempt: AttemptId,
    pub member: MemberId,
    pub ownership: OwnershipGeneration,
    pub backend_generation: u64,
}

impl TeamSession {
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

        let attempt = &self.room().attempts[index];

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

        let mut room = self.room().clone();
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

        let attempt = &self.room().attempts[index];

        if !matches!(&attempt.state, AttemptState::Accepted { provider_turn: accepted } if accepted == provider_turn)
            || attempt.intent.purpose == TurnPurpose::Summary
        {
            return Ok(None);
        }

        let mut room = self.room().clone();

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

        let attempt = &self.room().attempts[index];

        if !matches!(
            attempt.state,
            AttemptState::Sending | AttemptState::Accepted { .. } | AttemptState::Uncertain
        ) {
            return Ok(false);
        }

        let mut room = self.room().clone();

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

    pub(super) fn event_attempt(&self, key: AttemptEventKey) -> Option<usize> {
        let member = self.room().member(key.member)?;

        if member.ownership != key.ownership {
            return None;
        }

        self.room().attempts.iter().position(|attempt| {
            attempt.id == key.attempt
                && attempt.intent.recipient == key.member
                && attempt.intent.ownership == key.ownership
                && attempt.intent.backend_generation == key.backend_generation
        })
    }
}
