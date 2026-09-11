//! Picker presentation and executor integration for core branch operations.

pub(super) mod fork;
pub(super) mod rewind;
#[cfg(test)]
mod tests;

use gpui::Context;
use nmt_agent::session::branch::{BranchCompletion, FileProgress};

use crate::composer::CommandFeedbackKind;
use crate::{AgentPane, RecentSessionsMode, translated};

#[derive(Default)]
pub(crate) struct BranchFlow {
    draft: Option<String>,
    pending_prompt: Option<PendingBranchPrompt>,
}

struct PendingBranchPrompt {
    expected_draft: String,
    prompt: String,
}

impl BranchFlow {
    pub(crate) fn clear(&mut self) {
        self.draft = None;
        self.pending_prompt = None;
    }
}

impl AgentPane {
    pub(crate) fn branch_flow_holds_composer(&self) -> bool {
        self.session.branch.holds_composer()
    }

    pub(crate) fn complete_branch(&mut self, completion: BranchCompletion, cx: &mut Context<Self>) {
        let message = match (&completion.replay, completion.files) {
            (_, FileProgress::Restored) => "agent-rewind-complete-with-files",
            (Some(_), FileProgress::NotConfirmed) => "agent-rewind-complete",
            (None, FileProgress::NotConfirmed) => "agent-fork-complete",
        };
        let draft = self.branch.draft.take();
        self.clear_conversation_presentation(cx);
        self.history_ui.mode = RecentSessionsMode::Hidden;
        self.branch.pending_prompt = draft.map(|expected_draft| PendingBranchPrompt {
            expected_draft,
            prompt: completion.prompt,
        });
        if let Some(replay) = completion.replay {
            self.apply_replay(replay, cx);
        }
        self.palette
            .set_feedback(CommandFeedbackKind::Notice, translated(message), cx);
        cx.notify();
    }
}
