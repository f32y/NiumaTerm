use std::time::Instant;

use crate::session::controller::SessionController;
use crate::session::delivery::RecoverablePrompt;
use crate::session::input::{ApprovalOutcome, QuestionAction, QuestionKey, Submission};
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

impl SessionController {
    /// Recovery removes the provisional turn. Capture the interrupted turn
    /// first so a late completion can still be attributed to the user's stop.
    pub fn interrupt_from_user(&mut self) -> UserInterruption {
        let turn = self.delivery.is_active().then_some(self.delivery.turn());
        let prompt = self.delivery.take_interrupted_prompt();
        let outcome = self.runtime.interrupt(turn);
        UserInterruption { prompt, outcome }
    }

    pub fn respond_approval(&mut self, decision: &str) -> ApprovalOutcome {
        self.input.respond_approval(&mut self.runtime, decision)
    }

    pub fn submit_question(
        &mut self,
        key: QuestionKey,
        action: QuestionAction,
        now: Instant,
    ) -> QuestionSubmission {
        if self.branch.holds_composer() || self.commands.awaiting_turn {
            return QuestionSubmission::Ignored;
        }

        let waiting = self.input.waiting();
        match self
            .input
            .submit(&mut self.runtime, key, action, &self.controls.settings, now)
        {
            Submission::Ignored => QuestionSubmission::Ignored,
            Submission::Settled => QuestionSubmission::Settled {
                waiting_finished: waiting && !self.input.waiting(),
            },
            Submission::Waiting => QuestionSubmission::Waiting,
            Submission::Failed => QuestionSubmission::Failed,
        }
    }

    pub fn restore_questions(&mut self) {
        self.input.restore(&mut self.runtime);
    }
}
