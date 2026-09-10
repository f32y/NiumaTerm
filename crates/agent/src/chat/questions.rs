//! Questions retain their own identity because several batches can outlive a turn.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QuestionInput {
    #[default]
    SelectionOnly,
    Text,
    Secret,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub header: Option<String>,
    pub question: String,
    pub multi_select: bool,
    pub options: Vec<QuestionOption>,
    pub input: QuestionInput,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestionMode {
    Blocking,
    /// The request can be skipped after a visible countdown until the user interacts.
    Optional,
    /// Answers arrive as new user messages and remain valid after the asking turn ends.
    Async,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionRequest {
    pub id: String,
    pub mode: QuestionMode,
    pub questions: Vec<Question>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestionResolution {
    Submitted {
        message: Option<String>,
        started_turn: bool,
    },
    Skipped,
    Expired,
}
