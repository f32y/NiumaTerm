//! Coordinates one provider conversation without editors, windows, or rendering.
//!
//! The host schedules blocking work and displays returned outcomes. Runtime,
//! delivery, recovery, and interactions advance together under one owner.

use crate::chat::{SendOutcome, SlashCommandOutcome};
use crate::session::branch::ConversationBranch;
use crate::session::children::ChildAgents;
use crate::session::commands::{CommandQueue, PendingSlashCommand};
use crate::session::delivery::{MessageDelivery, RecoverablePrompt, Submission};
use crate::session::input::SessionInput;
use crate::session::lifecycle::{SessionRuntime, StartOutcome};
use crate::session::naming::ConversationNaming;
use crate::session::restore::ConversationRestore;
use crate::session::settings::ConversationSettings;
use crate::session::workflows::WorkflowData;
use crate::session::{AgentKind, Backend, RecoveryIdentity};

mod activity;
mod events;
mod input;
mod readiness;
mod transitions;

pub use crate::session::controller::events::SessionEffect;
pub use crate::session::controller::input::{QuestionSubmission, UserInterruption};
pub use crate::session::controller::readiness::{SessionReady, SessionReplay};
pub use crate::session::controller::transitions::{SessionFailure, SessionStart};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmissionBlock {
    QuestionResponse,
    ConversationChange,
    CommandStarting,
}

pub struct SessionController {
    pub runtime: SessionRuntime,
    pub delivery: MessageDelivery,
    pub restore: ConversationRestore,
    pub naming: ConversationNaming,
    pub controls: ConversationSettings,
    pub input: SessionInput,
    pub branch: ConversationBranch,
    pub commands: CommandQueue,
    pub children: ChildAgents,
    pub workflows: WorkflowData,
}

impl SessionController {
    pub fn new(kind: AgentKind) -> Self {
        Self {
            runtime: SessionRuntime::default(),
            delivery: MessageDelivery::new(kind),
            restore: ConversationRestore::default(),
            naming: ConversationNaming::default(),
            controls: ConversationSettings {
                seed_thread_defaults: true,
                ..ConversationSettings::default()
            },
            input: SessionInput::default(),
            branch: ConversationBranch::default(),
            commands: CommandQueue::default(),
            children: ChildAgents::default(),
            workflows: WorkflowData::default(),
        }
    }

    /// Rejected sends leave accepted work and recovery data unchanged.
    pub fn submit(
        &mut self,
        text: String,
        send: impl FnOnce(&mut Backend, &str) -> SendOutcome,
        recovery: impl FnOnce() -> Option<RecoverablePrompt>,
    ) -> Result<Submission, SubmissionBlock> {
        if self.input.has_submission() {
            return Err(SubmissionBlock::QuestionResponse);
        }
        if self.branch.holds_composer() {
            return Err(SubmissionBlock::ConversationChange);
        }
        if self.commands.awaiting_turn {
            return Err(SubmissionBlock::CommandStarting);
        }

        self.naming.sync(self.runtime.backend_mut());
        let outcome = self.runtime.send(|backend| send(backend, &text));
        Ok(self.delivery.submit(outcome, text, recovery))
    }

    pub fn execute_command(&mut self, command: &PendingSlashCommand) -> SlashCommandOutcome {
        self.commands.execute(self.runtime.backend_mut(), command)
    }

    fn settle_command(&mut self, outcome: &SlashCommandOutcome) -> bool {
        self.commands.settle(outcome, self.runtime.status())
    }

    /// Installation failure must retire accepted work from the failed start too.
    pub fn install(&mut self, epoch: u64, spawned: Result<Backend, String>) -> StartOutcome {
        let outcome = self.runtime.install(epoch, spawned);
        if matches!(outcome, StartOutcome::Failed(_)) {
            self.commands.clear();
            self.delivery.start_failed();
        }
        outcome
    }

    pub fn starting(&mut self, recovery: Option<&RecoveryIdentity>) -> SessionStart {
        let epoch = self.runtime.begin_start();
        self.input.starting(epoch);
        self.restore.starting(epoch, recovery);
        let reset_branch = !self.branch.starting(epoch, recovery);
        if reset_branch {
            self.branch.clear();
        }
        self.naming.named = recovery.is_some();
        SessionStart {
            epoch,
            reset_branch,
        }
    }
}
