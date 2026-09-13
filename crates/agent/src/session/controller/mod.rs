//! Coordinates one provider conversation without editors, windows, or rendering.
//!
//! The host schedules blocking work and displays returned outcomes. Runtime,
//! delivery, recovery, and interactions advance together under one owner.

pub use crate::session::controller::events::SessionEffect;
pub use crate::session::controller::input::{QuestionSubmission, UserInterruption};
pub use crate::session::controller::readiness::{SessionBranch, SessionReady, SessionReplay};
pub use crate::session::controller::transitions::{SessionFailure, SessionStart};

mod activity;
mod content;
mod events;
mod input;
mod readiness;
mod transitions;

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

use crate::chat::{
    GoalStatus, Item, SendOutcome, SkillCatalog, SlashCommandInfo, SlashCommandOutcome,
    ThreadSettings,
};
use crate::session::branch::ConversationBranch;
use crate::session::children::ChildAgents;
use crate::session::commands::{CommandQueue, PendingSlashCommand};
use crate::session::delivery::{MessageDelivery, RecoverablePrompt, Submission};
use crate::session::input::SessionInput;
use crate::session::lifecycle::{SessionRuntime, StartOutcome, Status};
use crate::session::naming::ConversationNaming;
use crate::session::restore::{ConversationRestore, SettingsSeed};
use crate::session::settings::ConversationSettings;
use crate::session::workflows::WorkflowData;
use crate::session::{AgentKind, Backend, RecoveryIdentity};
use crate::transcript::conversation::{ConversationImage, ConversationState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmissionBlock {
    QuestionResponse,
    ConversationChange,
    CommandStarting,
}

#[derive(Clone, Default)]
pub struct ReadyDefaults {
    pub stored: Option<ThreadSettings>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

pub struct SessionController {
    kind: AgentKind,
    pub ready_defaults: ReadyDefaults,
    pub goal: Option<GoalStatus>,
    pub plan_mode: bool,
    pub command_catalog: Option<Vec<SlashCommandInfo>>,
    pub skill_catalog: Option<SkillCatalog>,
    pub conversation: Rc<RefCell<ConversationState>>,
    pub pending_images: VecDeque<(String, Vec<Arc<ConversationImage>>)>,
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
            kind,
            ready_defaults: ReadyDefaults::default(),
            goal: None,
            plan_mode: false,
            command_catalog: None,
            skill_catalog: None,
            conversation: Rc::new(RefCell::new(ConversationState::default())),
            pending_images: VecDeque::new(),
            runtime: SessionRuntime::default(),
            delivery: MessageDelivery::new(kind),
            restore: ConversationRestore::default(),
            naming: ConversationNaming::default(),
            controls: ConversationSettings {
                seed: SettingsSeed::Defaults,
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

        let result = self.delivery.submit(outcome, text, recovery);

        if let Submission::Started { text } = &result {
            self.conversation.borrow_mut().start();

            self.push_item(Item::UserMessage {
                text: Some(text.clone()),
            });
        }

        Ok(result)
    }

    pub fn execute_command(&mut self, command: &PendingSlashCommand) -> SlashCommandOutcome {
        self.commands.execute(self.runtime.backend_mut(), command)
    }

    pub fn next_command(&mut self) -> Option<(String, SlashCommandOutcome)> {
        if self.runtime.status() != Status::Idle
            || self.commands.awaiting_turn
            || self.branch.holds_composer()
            || self.input.waiting()
        {
            return None;
        }

        let command = self.commands.queue.pop_front()?;
        let outcome = self.execute_command(&command);

        if matches!(
            outcome,
            SlashCommandOutcome::Rejected { .. } | SlashCommandOutcome::NotReady
        ) {
            self.commands.queue.clear();
        }

        Some((command.name, outcome))
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
