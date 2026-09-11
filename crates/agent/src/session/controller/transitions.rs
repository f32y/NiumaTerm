use crate::session::branch::BranchFailure;
use crate::session::controller::SessionController;

pub struct SessionStart {
    pub epoch: u64,
    pub reset_branch: bool,
}

pub struct SessionFailure {
    pub branch: Option<BranchFailure>,
    pub resume_failed: bool,
    pub cancelled_commands: bool,
}

impl SessionController {
    /// A provider or command may start work without a locally submitted prompt.
    /// Only that case reserves another turn; acknowledgements keep its number.
    pub(super) fn turn_started(&mut self) -> bool {
        let opened = if self.commands.turn_started() {
            self.delivery.begin_turn();

            true
        } else {
            self.delivery.provider_started()
        };

        self.runtime.turn_started();

        if opened {
            self.conversation.borrow_mut().start();
        }

        self.publish_confirmed();

        opened
    }

    /// Completion consumes the matching interrupt and releases both work queues.
    pub(super) fn turn_completed(&mut self) -> bool {
        let interrupted = self.runtime.turn_completed(self.delivery.turn());

        self.commands.turn_completed();
        self.delivery.completed();
        self.publish_confirmed();

        let turn = self.delivery.turn();
        let mut conversation = self.conversation.borrow_mut();

        if interrupted {
            conversation.turns.mark_interrupted(turn);
        }

        conversation.settle(turn);

        interrupted
    }

    pub fn failed(&mut self, message: &str, fatal: bool) -> SessionFailure {
        let branch = self.branch.failed(&mut self.runtime, message.to_owned());
        let resume_failed = self.restore.failed(&mut self.runtime);
        let cancelled_commands = fatal && !self.commands.queue.is_empty();

        if fatal {
            self.input.disconnect();
            self.runtime.exited(message);
            self.delivery.exited();
            self.commands.clear();
        } else if self.commands.awaiting_turn {
            self.commands.turn_completed();
        }

        SessionFailure {
            branch,
            resume_failed,
            cancelled_commands,
        }
    }

    pub fn disconnected(&mut self, message: &str) -> SessionFailure {
        let branch = self.branch.failed(
            &mut self.runtime,
            "session exited before branch readiness".into(),
        );

        let resume_failed = self.restore.failed(&mut self.runtime);

        self.runtime.exited(message);

        let cancelled_commands = self.commands.clear();

        self.input.disconnect();
        self.delivery.exited();

        SessionFailure {
            branch,
            resume_failed,
            cancelled_commands,
        }
    }

    /// Retain the backend while retiring conversation-specific state. Restarted
    /// turn numbers must not match an interrupt requested for the old content.
    pub fn clear_conversation(&mut self) {
        self.conversation.borrow_mut().clear();
        self.delivery.reset();
        self.pending_images.clear();
        self.runtime.clear_turn();
        self.branch.clear();
        self.restore.cancel();
        self.input.dismiss_approval();
        self.input.clear_questions();
        self.children.background_tasks = None;

        for child in self.children.transcripts.values() {
            child.conversation.borrow_mut().clear();
        }

        self.children.transcripts.clear();
        self.goal = None;
        self.plan_mode = false;
        self.workflows.clear();
    }
}
