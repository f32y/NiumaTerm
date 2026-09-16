//! The room's data model: the identifiers every record is keyed by, the
//! public content members exchange, and the context-selection types that
//! decide what a member is shown. They are one file because the storage
//! journal, the room, and the app's team views all read them together.

use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::{Error as UuidError, Uuid};

use crate::team::member::AcceptedCoverage;

macro_rules! identities {
    ($($name:ident),+ $(,)?) => {
        $(
            #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
            #[serde(transparent)]
            pub struct $name(Uuid);

            impl $name {
                pub fn new() -> Self {
                    Self(Uuid::new_v4())
                }
            }

            impl Default for $name {
                fn default() -> Self {
                    Self::new()
                }
            }

            impl FromStr for $name {
                type Err = UuidError;
                fn from_str(value: &str) -> Result<Self, Self::Err> {
                    Uuid::parse_str(value).map(Self)
                }
            }

            impl Display for $name {
                fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                    self.0.fmt(formatter)
                }
            }
        )+
    };
}

identities!(
    RoomId,
    MemberId,
    DiscussionId,
    StageId,
    OperationId,
    SummaryId,
    AttemptId,
    MessageId,
    InteractionId,
    AttachmentId,
);

/// A transfer advances ownership independently of provider restarts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OwnershipGeneration(u64);

impl Default for OwnershipGeneration {
    fn default() -> Self {
        Self(1)
    }
}

impl OwnershipGeneration {
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

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

pub struct ContextLimits {
    pub max_bytes: usize,
    pub recent_messages: usize,
}

pub struct PreparedContext {
    pub text: String,
    pub coverage: AcceptedCoverage,
    pub attachments: Vec<AttachmentReference>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryChunk {
    pub sources: Vec<MessageId>,
    pub fragments: Vec<SourceFragment>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContextError {
    #[error("selected public source or member is missing")]
    MissingSource,
    #[error("select a smaller public context range")]
    SelectRange,
    #[error("public context exceeds the delivery limit and automatic summaries are unavailable")]
    SummaryUnavailable,
    #[error("the user request exceeds the delivery limit")]
    OversizedInput,
    #[error("public context could not be encoded")]
    Encoding,
}
