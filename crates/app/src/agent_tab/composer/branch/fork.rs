//! Branching a conversation in front of a chosen prompt, for the backends
//! whose history lives behind their connection.
//!
//! Claude's history is a file this side reads and rewrites itself, and its
//! rewind picker offers the same cut alongside restoring the files that turn
//! touched; `/fork` opens that picker there rather than a second one that
//! would do less. What is left here is the shape the other two share: ask the
//! backend which prompts it can branch in front of, show them, and hand the
//! chosen one back so the branch starts where it was cut.

pub(crate) use nmt_agent::session::branch::PromptTarget;
#[cfg(test)]
pub(crate) use nmt_agent::session::branch::checkpoint_at_depth;

use gpui::SharedString;
use nmt_agent::session::branch::BranchView;
use rust_i18n::t;

use crate::agent_tab::composer::{
    PaletteAction, PaletteModel, PaletteRow, rewind_prompt_label, rewind_timestamp,
};

/// Name the prompt one picker row stands for.
///
/// Both pickers list their branch points newest first and append their cancel
/// row last, which is the same order `depth` counts in, so a row's own index
/// is the depth of the prompt it offers. The text travels with it, so the
/// transcript can refuse to move if the two lists have drifted apart.
pub(crate) fn row_prompt_target(row: usize, action: &PaletteAction) -> Option<PromptTarget> {
    let prompt = match action {
        PaletteAction::RewindCheckpoint(checkpoint) => checkpoint.prompt.clone(),
        PaletteAction::ForkCheckpoint(checkpoint) => checkpoint.prompt.clone(),
        _ => return None,
    };

    Some(PromptTarget { prompt, depth: row })
}

/// The fork picker for `state`: the prompts a branch can start in front of,
/// newest first, and the row that cancels.
pub(crate) fn fork_palette_model(state: BranchView<'_>) -> Option<PaletteModel> {
    match state {
        BranchView::LoadingFork => Some(PaletteModel {
            rows: vec![cancel_row()],
            note: Some(SharedString::from(t!("agent-fork-loading-checkpoints"))),
        }),
        BranchView::ForkCheckpoints(checkpoints) => {
            let mut rows = checkpoints
                .iter()
                .cloned()
                .map(|checkpoint| PaletteRow {
                    label: rewind_prompt_label(&checkpoint.prompt).into(),
                    description: SharedString::from(t!("agent-fork-branch-before-prompt")),
                    hint: rewind_timestamp(checkpoint.timestamp.as_deref()).map(Into::into),
                    disabled_reason: None,
                    action: PaletteAction::ForkCheckpoint(checkpoint),
                })
                .collect::<Vec<_>>();

            rows.push(cancel_row());

            Some(PaletteModel {
                rows,
                note: Some(SharedString::from(t!("agent-fork-choose-prompt"))),
            })
        }
        _ => None,
    }
}

fn cancel_row() -> PaletteRow {
    PaletteRow {
        label: SharedString::from(t!("agent-fork-cancel")),
        description: SharedString::from(t!("agent-fork-cancel-description")),
        hint: None,
        disabled_reason: None,
        action: PaletteAction::ForkCancel,
    }
}
