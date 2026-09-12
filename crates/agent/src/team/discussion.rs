use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::team::budget::Budget;
use crate::team::content::UserInput;
use crate::team::identity::{
    AttemptId, DiscussionId, InteractionId, MemberId, MessageId, OperationId, StageId, SummaryId,
};
use crate::team::moderation::ModeratorDecision;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiscussionMode {
    Fixed { report_author: MemberId },
    Moderated { moderator: MemberId },
}

impl DiscussionMode {
    pub fn report_author(self) -> MemberId {
        match self {
            Self::Fixed { report_author } => report_author,
            Self::Moderated { moderator } => moderator,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscussionState {
    Idle,
    Running,
    Pausing,
    Paused,
    Finishing,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum PauseReason {
    User,
    UserInput(MessageId),
    ModeChange,
    MemberUnavailable(MemberId),
    ModeratorUnavailable,
    Interaction(InteractionId),
    AttemptFailed(AttemptId),
    UncertainAttempt(AttemptId),
    SummaryFailed(AttemptId),
    InvalidModeration(OperationId),
    Budget,
    ContextSelection,
    DispatchUnavailable,
    Storage,
    Maintenance(String),
    Transfer(MemberId),
    Reopened,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    InitialAnswers,
    PeerResponses,
    ModeratorDecision,
    InvitedResponses,
    Report,
    Direct,
    Summary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "attempt", rename_all = "snake_case")]
pub enum ArrangementState {
    Pending,
    Active(AttemptId),
    Completed(AttemptId),
    Failed(AttemptId),
    Uncertain(AttemptId),
    Skipped,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arrangement {
    pub operation: OperationId,
    pub recipient: MemberId,
    pub state: ArrangementState,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicSnapshot {
    pub messages: Vec<MessageId>,
    pub summaries: Vec<SummaryId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stage {
    pub id: StageId,
    pub kind: StageKind,
    pub arrangements: Vec<Arrangement>,
    pub segments: Vec<PublicSnapshot>,
    pub decision: Option<ModeratorDecision>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discussion {
    pub(in crate::team) id: DiscussionId,
    pub(in crate::team) objective: String,
    pub(in crate::team) participants: Vec<MemberId>,
    pub(in crate::team) mode: DiscussionMode,
    pub(in crate::team) state: DiscussionState,
    pub(in crate::team) pauses: BTreeSet<PauseReason>,
    pub(in crate::team) stages: Vec<Stage>,
    pub(in crate::team) budget: Budget,
    #[serde(default)]
    pub(in crate::team) request: UserInput,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum DiscussionError {
    #[error("select distinct members and an available report author or moderator")]
    InvalidParticipants,

    #[error("this room already has an active discussion")]
    AlreadyActive,

    #[error("wait for current work to settle before changing discussion mode")]
    StillRunning,

    #[error("this discussion has completed")]
    Completed,
}

impl Discussion {
    pub(in crate::team) fn new(
        objective: String,
        participants: Vec<MemberId>,
        mode: DiscussionMode,
    ) -> Result<Self, DiscussionError> {
        let unique: BTreeSet<_> = participants.iter().copied().collect();

        if unique.is_empty() || unique.len() != participants.len() {
            return Err(DiscussionError::InvalidParticipants);
        }

        Ok(Self {
            id: DiscussionId::new(),
            request: UserInput {
                text: objective.clone(),
                ..UserInput::default()
            },
            objective,
            participants,
            mode,
            state: DiscussionState::Idle,
            pauses: BTreeSet::new(),
            stages: Vec::new(),
            budget: Budget::discussion(),
        })
    }

    pub fn id(&self) -> DiscussionId {
        self.id
    }

    pub fn objective(&self) -> &str {
        &self.objective
    }

    pub fn participants(&self) -> &[MemberId] {
        &self.participants
    }

    pub fn mode(&self) -> DiscussionMode {
        self.mode
    }

    pub fn state(&self) -> DiscussionState {
        self.state
    }

    pub fn pauses(&self) -> &BTreeSet<PauseReason> {
        &self.pauses
    }

    pub fn stages(&self) -> &[Stage] {
        &self.stages
    }

    pub fn budget(&self) -> &Budget {
        &self.budget
    }

    pub fn pause(&mut self, reason: PauseReason) -> bool {
        if self.state == DiscussionState::Completed {
            return false;
        }

        let inserted = self.pauses.insert(reason);

        self.settle_pause();

        inserted
    }

    pub(in crate::team) fn settle_pause(&mut self) {
        self.state = if self.has_active_work() {
            DiscussionState::Pausing
        } else {
            DiscussionState::Paused
        };
    }

    /// Resolving one condition leaves the explicit continuation gate closed.
    pub fn resolve_pause(&mut self, reason: &PauseReason) -> bool {
        self.pauses.remove(reason)
    }

    pub(in crate::team) fn change_mode(
        &mut self,
        mode: DiscussionMode,
    ) -> Result<(), DiscussionError> {
        if self.state == DiscussionState::Completed {
            return Err(DiscussionError::Completed);
        }

        self.pause(PauseReason::ModeChange);

        if self.has_active_work() {
            return Err(DiscussionError::StillRunning);
        }

        self.mode = mode;

        Ok(())
    }

    fn has_active_work(&self) -> bool {
        self.stages
            .iter()
            .flat_map(|stage| &stage.arrangements)
            .any(|arrangement| {
                matches!(
                    arrangement.state,
                    ArrangementState::Active(_) | ArrangementState::Uncertain(_)
                )
            })
    }
}
