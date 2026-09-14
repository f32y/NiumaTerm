//! Picker presentation and executor integration for core branch operations.

pub(super) mod fork;
pub(super) mod rewind;

#[cfg(test)]
mod tests;

use gpui::{Context, Entity, Window};
use gpui_component::input::TextareaState;

use crate::agent_tab::AgentPane;

#[derive(Default)]
pub(crate) struct BranchFlow {
    pub(crate) draft: Option<String>,
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

    /// Ready may arrive without a window. The next render applies the prompt
    /// only if the editor still contains the draft captured for this operation.
    pub(crate) fn fill_branch_prompt(
        &mut self,
        input: &Entity<TextareaState>,
        window: &mut Window,
        cx: &mut Context<AgentPane>,
    ) {
        let Some(pending) = self.pending_prompt.take() else {
            return;
        };

        if *input.read(cx).text() != pending.expected_draft {
            return;
        }

        input.update(cx, |input, cx| {
            let end = pending.prompt.len();

            input.set_value(pending.prompt, window, cx);

            input.set_selected_range(end..end, cx);
        });
    }

    pub(crate) fn prepare_prompt(&mut self, draft: Option<String>, prompt: String) {
        self.pending_prompt = draft.map(|expected_draft| PendingBranchPrompt {
            expected_draft,
            prompt,
        });
    }

    pub(crate) fn reset_pending_prompt(&mut self) {
        self.pending_prompt = None;
    }
}
