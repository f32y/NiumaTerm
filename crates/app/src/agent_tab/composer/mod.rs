pub(super) use nmt_agent::session::commands::PendingSlashCommand;

pub(super) use crate::agent_tab::composer::branch::BranchFlow;
#[cfg(test)]
pub(super) use crate::agent_tab::composer::branch::fork::checkpoint_at_depth;
pub(super) use crate::agent_tab::composer::branch::fork::{PromptTarget, row_prompt_target};
pub(super) use crate::agent_tab::composer::branch::rewind::{
    RewindAction, rewind_prompt_label, rewind_timestamp,
};
pub(super) use crate::agent_tab::composer::palette::{
    CachedCatalog, PALETTE_MAX_HEIGHT, PaletteAction, PaletteModel, PaletteRow, SlashPalette,
};
pub(super) use crate::agent_tab::composer::response_annotations::{
    annotation_count_label, parse_annotated_prompt, prompt_with_response_annotations,
    visible_prompt,
};

pub(super) mod attachments;

mod branch;
mod palette;
mod response_annotations;

#[cfg(test)]
mod tests;

use gpui::SharedString;

#[cfg(test)]
use crate::agent_tab::composer::palette::{feedback_is_current, feedback_is_transient};
use crate::agent_tab::session::Status;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandFeedbackKind {
    Notice,
    /// Information the user asked to see, or work still under way. Neither is
    /// an acknowledgement of something already done, so both hold until a
    /// newer message replaces them.
    Status,
    Error,
    Queued,
}

#[derive(Clone)]
pub(super) struct CommandFeedback {
    pub(super) kind: CommandFeedbackKind,

    /// Redrawn on every frame the composer paints while the message is up, so
    /// it is stored in the form the view hands to `child` rather than copied
    /// into one each time.
    pub(super) message: SharedString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ComposerAction {
    Send,
    Stop,
}

pub(super) fn restored_input_after_interruption(submitted: &str, current: &str) -> String {
    if current.trim().is_empty() || current == submitted {
        submitted.to_string()
    } else {
        format!("{submitted}\n\n{current}")
    }
}

impl From<Status> for ComposerAction {
    fn from(status: Status) -> Self {
        if status == Status::Running {
            ComposerAction::Stop
        } else {
            ComposerAction::Send
        }
    }
}
