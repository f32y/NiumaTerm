use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::team::attempt::{Attempt, AttemptState, BudgetScope};
use crate::team::budget::{Budget, TurnPurpose};
use crate::team::model::{
    AttemptId, DiscussionId, InteractionId, MemberId, MessageId, OperationId, StageId, SummaryId,
    UserInput,
};

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
    Reopened,
    Closed,
}

impl PauseReason {
    /// The pause for attempt `attempt`, sent for `purpose`, failing with a
    /// known outcome. A failed summary is reported apart from a failed turn.
    pub(super) fn attempt_failed(attempt: AttemptId, purpose: TurnPurpose) -> Self {
        if purpose == TurnPurpose::Summary {
            Self::SummaryFailed(attempt)
        } else {
            Self::AttemptFailed(attempt)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    InitialAnswers,
    PeerResponses,
    ModeratorDecision,
    InvitedResponses,
    Report,
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

impl Stage {
    /// A new stage of `kind` opened on the public `snapshot`, with a pending
    /// arrangement for each of `recipients`.
    pub(super) fn pending(
        kind: StageKind,
        recipients: Vec<MemberId>,
        snapshot: PublicSnapshot,
    ) -> Self {
        Self {
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
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discussion {
    pub(super) id: DiscussionId,
    pub(super) objective: String,
    pub(super) participants: Vec<MemberId>,
    pub(super) mode: DiscussionMode,
    pub(super) state: DiscussionState,
    pub(super) pauses: BTreeSet<PauseReason>,
    pub(super) stages: Vec<Stage>,
    pub(super) budget: Budget,
    #[serde(default)]
    pub(super) request: UserInput,
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
    pub fn remaining_non_report_turns(&self, attempts: &[Attempt]) -> u32 {
        self.budget
            .remaining_non_report_turns(attempts.iter().filter(|attempt| {
                attempt.intent.budget == BudgetScope::Discussion(self.id)
                    && attempt.state != AttemptState::Rejected
            }))
    }

    pub(super) fn new(
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

    pub fn pause(&mut self, reason: PauseReason) -> bool {
        if self.state == DiscussionState::Completed {
            return false;
        }

        let inserted = self.pauses.insert(reason);

        self.settle_pause();

        inserted
    }

    pub(super) fn settle_pause(&mut self) {
        self.state = if self.has_active_work() {
            DiscussionState::Pausing
        } else {
            DiscussionState::Paused
        };
    }

    /// Whether work may be sent for this discussion: it is running or
    /// finishing, and nothing holds it paused.
    pub(super) fn is_dispatchable(&self) -> bool {
        self.pauses.is_empty()
            && matches!(
                self.state,
                DiscussionState::Running | DiscussionState::Finishing
            )
    }

    /// Set every arrangement made for `operation` to `state`.
    pub(super) fn mark_operation(&mut self, operation: OperationId, state: ArrangementState) {
        for arrangement in self
            .stages
            .iter_mut()
            .flat_map(|stage| &mut stage.arrangements)
        {
            if arrangement.operation == operation {
                arrangement.state = state;
            }
        }
    }

    /// Resolving one condition leaves the explicit continuation gate closed.
    pub fn resolve_pause(&mut self, reason: &PauseReason) -> bool {
        self.pauses.remove(reason)
    }

    pub(super) fn change_mode(&mut self, mode: DiscussionMode) -> Result<(), DiscussionError> {
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
