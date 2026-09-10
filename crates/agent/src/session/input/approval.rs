use crate::session::input::SessionInput;
use crate::session::{Backend, SessionRuntime};

pub(super) struct Approval {
    description: String,
    submitted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Ignored,
    Rejected,
    Waiting,
    Settled,
}

impl SessionInput {
    pub fn approval(&self) -> Option<&str> {
        self.approval
            .as_ref()
            .map(|approval| approval.description.as_str())
    }

    pub fn ask_approval(&mut self, description: String) {
        self.approval = Some(Approval {
            description,
            submitted: false,
        });
    }

    pub fn dismiss_approval(&mut self) {
        self.approval = None;
    }

    pub fn resolve_approval(&mut self, epoch: u64) -> bool {
        epoch == self.epoch && !self.disconnected && self.approval.take().is_some()
    }

    pub fn respond_approval(
        &mut self,
        runtime: &mut SessionRuntime,
        decision: &str,
    ) -> ApprovalOutcome {
        if self.epoch != runtime.epoch() || self.disconnected {
            return ApprovalOutcome::Ignored;
        }
        let Some(approval) = &mut self.approval else {
            return ApprovalOutcome::Ignored;
        };
        if approval.submitted {
            return ApprovalOutcome::Ignored;
        }
        let Some(backend) = runtime.backend_mut() else {
            return ApprovalOutcome::Rejected;
        };
        let waits = match backend {
            Backend::DeepSeek(_) => true,
            Backend::Codex(_) | Backend::Claude(_) => false,
            #[cfg(any(test, feature = "test-support"))]
            Backend::Test(session) => session.approval_waits,
        };
        if !backend.respond_approval(decision) {
            return ApprovalOutcome::Rejected;
        }
        if waits {
            approval.submitted = true;
            ApprovalOutcome::Waiting
        } else {
            self.approval = None;
            ApprovalOutcome::Settled
        }
    }
}
