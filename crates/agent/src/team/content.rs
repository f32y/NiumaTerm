use serde::{Deserialize, Serialize};

use crate::team::identity::{AttachmentId, MemberId, MessageId, SummaryId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentReference {
    pub id: AttachmentId,
    pub media_type: String,
    pub bytes: u64,
    pub digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Author {
    User,
    Member { id: MemberId, name: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Publication {
    UserInput,
    RootReply,
    ExplicitShare,
    ModeratorDecision,
    Report,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicMessage {
    pub id: MessageId,
    pub author: Author,
    pub publication: Publication,
    pub text: String,
    pub replies_to: Vec<MessageId>,
    pub attachments: Vec<AttachmentReference>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInput {
    pub text: String,
    pub references: Vec<MessageId>,
    pub attachments: Vec<AttachmentReference>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributedPosition {
    pub member: MemberId,
    pub position: String,
    pub reasons: String,
    pub sources: Vec<MessageId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFragment {
    pub source: MessageId,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub id: SummaryId,
    pub version: u32,
    pub owner: MemberId,
    pub sources: Vec<MessageId>,
    pub fragments: Vec<SourceFragment>,
    pub prior_summaries: Vec<SummaryId>,
    pub goals: String,
    pub constraints: String,
    pub agreements: String,
    pub disagreements: Vec<AttributedPosition>,
}
