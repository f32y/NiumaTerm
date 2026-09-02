//! Cutting the conversation at an earlier point.
//!
//! A rewind and a fork are two answers to the same question: which earlier
//! prompt should the conversation continue from. Both open a picker over the
//! same checkpoint list, both replace the session under the composer once the
//! user picks, and each cancels the other, so neither can be reasoned about
//! without the other.

pub(super) mod fork;
pub(super) mod rewind;

use crate::AgentPane;
use crate::composer::branch::fork::{ForkFlow, ForkState};
use crate::composer::branch::rewind::{RewindFlow, RewindState, rewind_blocks_submission};

/// The two ways of cutting the conversation, held together because at most one
/// of them runs at a time and every question the composer asks about either is
/// really a question about both.
#[derive(Default)]
pub(crate) struct BranchFlow {
    pub(crate) rewind: RewindFlow,
    pub(crate) fork: ForkFlow,
}

impl BranchFlow {
    /// Whether a branch or a rewind is holding the composer.
    ///
    /// Both replace the conversation under it, so a prompt sent while either
    /// is open would reach a session about to be swapped out, and while a
    /// picker is showing, the keys that would send it are the picker's.
    pub(crate) fn holds_composer(&self) -> bool {
        rewind_blocks_submission(self.rewind.state.as_ref()) || self.fork.state.is_some()
    }

    /// Whether such a flow is past its picker and working. Until then the
    /// input still holds text worth editing, so only sending is refused.
    pub(crate) fn is_working(&self) -> bool {
        self.rewind
            .state
            .as_ref()
            .is_some_and(|state| !state.is_picker())
            || self
                .fork
                .state
                .as_ref()
                .is_some_and(|state| !state.is_picker())
    }

    /// Whether a list of branch points is on screen, which is what makes the
    /// palette's highlight something the transcript follows.
    pub(crate) fn picker_is_open(&self) -> bool {
        self.rewind
            .state
            .as_ref()
            .is_some_and(RewindState::is_picker)
            || self.fork.state.as_ref().is_some_and(ForkState::is_picker)
    }

    /// Drop both flows, as a session replacement does.
    pub(crate) fn clear(&mut self) {
        self.rewind.state = None;
        self.rewind.file_completion = None;
        self.fork.state = None;
    }
}

impl AgentPane {
    /// Whether a branch or a rewind is holding the composer.
    pub(crate) fn branch_flow_holds_composer(&self) -> bool {
        self.branch.holds_composer()
    }
}
