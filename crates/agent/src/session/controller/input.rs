use crate::session::delivery::RecoverablePrompt;
use crate::session::lifecycle::InterruptOutcome;

pub struct UserInterruption {
    pub prompt: Option<(u64, RecoverablePrompt)>,
    pub outcome: InterruptOutcome,
}

pub enum QuestionSubmission {
    Ignored,
    Settled { waiting_finished: bool },
    Waiting,
    Failed,
}
