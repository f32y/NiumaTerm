use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::team::attempt::{AttemptState, BudgetScope};
use crate::team::budget::TurnPurpose;
use crate::team::discussion::{DiscussionMode, PauseReason, StageKind};
use crate::team::identity::{AttemptId, MemberId, OperationId, StageId};
use crate::team::session::{AttemptEventKey, TeamError, TeamSession};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", deny_unknown_fields, rename_all = "snake_case")]
pub enum ModeratorAction {
    Invite { recipients: Vec<MemberId> },
    Report,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeratorDecision {
    pub operation: OperationId,
    pub attempt: AttemptId,
    pub actor: MemberId,
    pub action: ModeratorAction,
}

impl TeamSession {
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

        let mut room = self.room().clone();

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

        self.commit_room(room)?;

        Ok(true)
    }
}
