use crate::chat::{ReplayTurn, ThreadSettings};
use crate::session::AgentKind;
use crate::session::branch::{BranchCompletion, BranchReplay};
use crate::session::capabilities::AgentCapabilities as _;
use crate::session::controller::SessionController;
use crate::session::restore::{ReadyAction, ReplayAction};

pub struct SessionReady {
    pub settings: ThreadSettings,
    pub branch: Option<BranchCompletion>,
    pub replay: Option<Vec<ReplayTurn>>,
}

pub struct SessionReplay {
    pub turns: Vec<ReplayTurn>,
    pub branch: Option<BranchCompletion>,
    pub replace: bool,
}

impl SessionController {
    pub(super) fn prepare_ready(&mut self, settings: ThreadSettings) -> Option<SessionReady> {
        let epoch = self.runtime.epoch();
        let branch = self.branch.ready(epoch);
        if branch.is_some() {
            self.clear_conversation();
        }
        let replay = match self.restore.ready(epoch) {
            ReadyAction::Ignore => return None,
            ReadyAction::Apply => None,
            ReadyAction::Replay(replay) => {
                self.clear_conversation();
                Some(replay)
            }
        };
        Some(SessionReady {
            settings,
            branch,
            replay,
        })
    }

    pub(super) fn prepare_replay(&mut self, turns: Vec<ReplayTurn>) -> Option<SessionReplay> {
        let epoch = self.runtime.epoch();
        let branch = match self.branch.replayed(epoch) {
            BranchReplay::Ignore => return None,
            BranchReplay::Unrelated => None,
            BranchReplay::Complete(completion) => {
                // A completed branch supersedes any pending restore before the
                // incoming replay is classified for the new conversation.
                self.clear_conversation();
                Some(completion)
            }
        };
        let replace = match self.restore.replayed(epoch) {
            ReplayAction::Ignore => return None,
            ReplayAction::Append => false,
            ReplayAction::Replace => {
                self.clear_conversation();
                true
            }
        };
        Some(SessionReplay {
            turns,
            branch,
            replace,
        })
    }

    /// Apply host-supplied defaults after any restored content has been accepted.
    /// A model-selection refusal leaves the effective settings reported by the
    /// backend and returns its error for the host to present.
    pub fn finish_ready(
        &mut self,
        kind: AgentKind,
        settings: ThreadSettings,
        stored: Option<&ThreadSettings>,
        startup_model: Option<&str>,
        startup_effort: Option<&str>,
    ) -> Option<Result<(), String>> {
        self.input.restore(&mut self.runtime);
        self.controls
            .ready(kind, settings, stored, startup_model, startup_effort);
        let selection = if kind.caps().model_selection_is_a_request {
            self.runtime
                .backend_mut()
                .and_then(|backend| self.controls.apply_model(backend))
        } else {
            None
        };
        self.naming.sync(self.runtime.backend_mut());
        self.runtime.ready();
        selection
    }
}
