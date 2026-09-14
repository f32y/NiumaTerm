pub use crate::team::content::SourceFragment;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::team::content::AttachmentReference;
use crate::team::identity::MessageId;
use crate::team::member::AcceptedCoverage;

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
