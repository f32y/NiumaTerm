use serde::{Deserialize, Serialize};

use crate::team::budget::TurnPurpose;
use crate::team::content::{AttachmentReference, UserInput};
use crate::team::context::SummaryChunk;
use crate::team::discussion::PublicSnapshot;
use crate::team::identity::{
    AttemptId, DiscussionId, MemberId, MessageId, OperationId, OwnershipGeneration, StageId,
    SummaryId,
};
use crate::team::member::AcceptedCoverage;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum BudgetScope {
    Discussion(DiscussionId),
    Direct(OperationId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchIntent {
    pub invocation: Invocation,
    pub recipient: MemberId,
    pub ownership: OwnershipGeneration,
    pub backend_generation: u64,
    pub operation: OperationId,
    pub stage: Option<StageId>,
    pub budget: BudgetScope,
    pub purpose: TurnPurpose,
    pub input: UserInput,
    pub attachments: Vec<AttachmentReference>,
    pub prepared_text: String,
    pub snapshot: PublicSnapshot,
    pub coverage: AcceptedCoverage,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "sources", rename_all = "snake_case")]
pub enum Invocation {
    #[default]
    MemberConversation,
    PublicSummary(SummaryChunk),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "result", rename_all = "snake_case")]
pub enum AttemptState {
    Reserved,
    Sending,
    Accepted { provider_turn: String },
    Rejected,
    Completed { message: MessageId },
    Summarized { summary: SummaryId },
    Failed,
    Uncertain,
    Abandoned,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub id: AttemptId,
    pub intent: DispatchIntent,
    pub state: AttemptState,
    pub provider_turn: Option<String>,
}
