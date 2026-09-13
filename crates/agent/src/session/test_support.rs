use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::chat::{
    ForkAnchor, QuestionRequest, QuestionResponse, SendOutcome, SlashCommandInfo,
    SlashCommandOutcome,
};
use crate::session::team_recovery::RecoveredTeamTurn;
use crate::session::{AgentKind, RecoveryIdentity, RenameOutcome};
use crate::workflow::{WorkflowRefreshRequest, WorkflowSource};

#[derive(Debug, PartialEq, Eq)]
pub struct InputResponse {
    pub id: String,
    pub answers: Option<Vec<Vec<String>>>,
}

pub struct TestBackend {
    pub workflow_source: Option<Arc<dyn WorkflowSource>>,
    pub workflow_requests: Vec<WorkflowRefreshRequest>,
    pub approval_accepted: bool,
    pub approval_waits: bool,
    pub approval_responses: Vec<String>,
    pub input_result: Result<QuestionResponse, String>,
    pub input_responses: Vec<InputResponse>,
    pub restored_questions: Vec<QuestionRequest>,
    pub rename_outcome: RenameOutcome,
    pub interrupt_accepted: bool,
    pub resume_accepted: bool,
    pub fork_accepted: bool,
    pub fork_requests: Vec<ForkAnchor>,
    pub file_restore_requests: Vec<String>,
    pub team_recovered_turns: Vec<RecoveredTeamTurn>,
    pub(super) send_outcomes: VecDeque<SendOutcome>,
    pub(super) slash_outcome: SlashCommandOutcome,
    pub(super) commands: Vec<SlashCommandInfo>,

    /// Raised from `Drop` when a test needs to observe the moment the pane
    /// lets go of the session. A DeepSeek session's release is what can stop
    /// the shared host process, so when it happens is behavior of its own.
    released: Option<Arc<AtomicBool>>,

    pub(super) recovery: Option<RecoveryIdentity>,
}

impl TestBackend {
    pub fn new(
        send_outcomes: impl IntoIterator<Item = SendOutcome>,
        slash_outcome: SlashCommandOutcome,
        commands: Vec<SlashCommandInfo>,
    ) -> Self {
        Self {
            workflow_source: None,
            workflow_requests: Vec::new(),
            approval_accepted: false,
            approval_waits: false,
            approval_responses: Vec::new(),
            input_result: Ok(QuestionResponse::Pending),
            input_responses: Vec::new(),
            restored_questions: Vec::new(),
            rename_outcome: RenameOutcome::Unsupported,
            interrupt_accepted: false,
            resume_accepted: false,
            fork_accepted: false,
            fork_requests: Vec::new(),
            file_restore_requests: Vec::new(),
            team_recovered_turns: Vec::new(),
            send_outcomes: send_outcomes.into_iter().collect(),
            slash_outcome,
            commands,
            released: None,
            recovery: None,
        }
    }

    pub fn watch_release(mut self, released: Arc<AtomicBool>) -> Self {
        self.released = Some(released);

        self
    }

    pub fn with_recovery(mut self, kind: AgentKind, id: impl Into<String>) -> Self {
        self.recovery = Some(RecoveryIdentity::new(kind, id));

        self
    }
}

impl Drop for TestBackend {
    fn drop(&mut self) {
        if let Some(released) = &self.released {
            released.store(true, Ordering::SeqCst);
        }
    }
}
