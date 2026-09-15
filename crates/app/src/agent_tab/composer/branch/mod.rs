//! Picker presentation and executor integration for core branch operations.

pub(super) mod fork;
pub(super) mod rewind;

#[cfg(test)]
mod tests;

use gpui::{Context, Entity, Window};
use gpui_component::input::TextareaState;
use nmt_agent::session::branch::{BranchError, BranchFailure, FailureStage, FileProgress};
use rust_i18n::t;

use crate::agent_tab::AgentPane;
use crate::agent_tab::profile::AgentKind;
use crate::agent_tab::session::errors::operation_error;

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

/// What a failed branch operation tells the user, for a `kind` session.
pub(crate) fn branch_error_message(error: BranchError, kind: AgentKind) -> String {
    match error {
        BranchError::Busy => t!("agent-rewind-idle-only").to_string(),
        BranchError::NotReady => {
            t!("agent-session-still-starting", name = kind.display()).into_owned()
        }
        BranchError::MissingSession => t!("agent-rewind-no-session-id").to_string(),
        BranchError::FilesUnavailable => t!("agent-rewind-file-checkpoint-unavailable").to_string(),
        BranchError::InvalidFileResult(message) => {
            message.unwrap_or_else(|| t!("agent-rewind-invalid-file-state").to_string())
        }
        BranchError::Operation(error) => operation_error(error),
        BranchError::Failed(message) => message,
    }
}

/// What a branch operation that failed at `failure`'s stage tells the user,
/// including whether files were already restored when it stopped.
pub(crate) fn branch_failure_message(failure: BranchFailure, kind: AgentKind) -> String {
    let error = branch_error_message(failure.error, kind);

    match (failure.stage, failure.files) {
        (FailureStage::Checkpoints | FailureStage::ProtocolFork, _) => error,
        (FailureStage::Files, _) => t!("agent-rewind-file-failed", error = &error).into_owned(),
        (FailureStage::Conversation, FileProgress::Restored) => t!(
            "agent-rewind-conversation-failed-after-files",
            error = &error
        )
        .into_owned(),
        (FailureStage::Conversation, FileProgress::NotConfirmed) => {
            t!("agent-rewind-conversation-failed", error = &error).into_owned()
        }
        (FailureStage::Startup, FileProgress::Restored) => {
            t!("agent-rewind-start-failed-after-files").to_string()
        }
        (FailureStage::Startup, FileProgress::NotConfirmed) => {
            t!("agent-rewind-start-failed").to_string()
        }
    }
}
