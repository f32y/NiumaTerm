use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::chat::{SendOutcome, SlashCommandInfo, SlashCommandOutcome};
use crate::session::{AgentKind, RecoveryIdentity, RenameOutcome};

pub struct TestBackend {
    pub rename_outcome: RenameOutcome,
    pub interrupt_accepted: bool,
    pub resume_accepted: bool,
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
            rename_outcome: RenameOutcome::Unsupported,
            interrupt_accepted: false,
            resume_accepted: false,
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
