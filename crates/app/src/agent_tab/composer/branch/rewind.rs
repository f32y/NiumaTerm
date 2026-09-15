pub(crate) use nmt_agent::session::branch::RewindAction;

use std::borrow::Cow;

use chrono::Local;
use gpui::SharedString;
use nmt_agent::claude_code::sessions;
use nmt_agent::session::branch::{BranchView, FileProgress};
use rust_i18n::t;

use crate::agent_tab::composer::{PaletteAction, PaletteModel, PaletteRow};

pub(crate) fn rewind_prompt_label(prompt: &str) -> String {
    let line = prompt
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(Cow::Borrowed)
        .unwrap_or_else(|| t!("agent-rewind-untitled-prompt"));

    let line = line.trim();

    let mut label = line.chars().take(72).collect::<String>();

    if line.chars().count() > 72 {
        label.push('…');
    }

    label
}

pub(crate) fn rewind_timestamp(timestamp: Option<&str>) -> Option<String> {
    let timestamp = timestamp?;

    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|time| {
            time.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .ok()
        .or_else(|| Some(timestamp.to_string()))
}

/// The rewind picker for `state`: the prompts to return to, newest first,
/// then, once one is chosen, what to restore from it.
pub(crate) fn rewind_palette_model(state: BranchView<'_>) -> Option<PaletteModel> {
    match state {
        BranchView::LoadingRewind => Some(PaletteModel {
            rows: vec![rewind_cancel_row()],
            note: Some(SharedString::from(t!("agent-rewind-loading-active-branch"))),
        }),
        BranchView::RewindCheckpoints(checkpoints) => {
            let mut rows = checkpoints
                .iter()
                .cloned()
                .map(|checkpoint| PaletteRow {
                    label: rewind_prompt_label(&checkpoint.prompt).into(),
                    description: SharedString::from(t!("agent-rewind-return-before-prompt")),
                    hint: rewind_timestamp(checkpoint.timestamp.as_deref()).map(Into::into),
                    disabled_reason: None,
                    action: PaletteAction::RewindCheckpoint(checkpoint),
                })
                .collect::<Vec<_>>();

            rows.push(rewind_cancel_row());

            Some(PaletteModel {
                rows,
                note: Some(SharedString::from(t!("agent-rewind-choose-prompt"))),
            })
        }
        BranchView::RewindAction(checkpoint, files) => {
            let file_disabled = match checkpoint.file_restore_availability {
                sessions::FileRestoreAvailability::Unavailable => Some(SharedString::from(t!(
                    "agent-rewind-file-checkpoint-unavailable"
                ))),
                _ => None,
            };

            let file_description = match checkpoint.file_restore_availability {
                sessions::FileRestoreAvailability::Available => {
                    t!("agent-rewind-files-description-available")
                }
                sessions::FileRestoreAvailability::Unknown => {
                    t!("agent-rewind-files-description-unknown")
                }
                sessions::FileRestoreAvailability::Unavailable => {
                    t!("agent-rewind-files-description-unavailable")
                }
            };

            let files_only_disabled = match files {
                FileProgress::Restored => {
                    Some(SharedString::from(t!("agent-rewind-files-restored")))
                }
                FileProgress::NotConfirmed => file_disabled.clone(),
            };

            Some(PaletteModel {
                rows: vec![
                    PaletteRow {
                        label: SharedString::from(t!("agent-rewind-restore-files")),
                        description: SharedString::from(file_description),
                        hint: Some(SharedString::from(t!("agent-rewind-files-only"))),
                        disabled_reason: files_only_disabled,
                        action: PaletteAction::RewindAction(RewindAction::Files),
                    },
                    PaletteRow {
                        label: SharedString::from(t!("agent-rewind-restore-conversation")),
                        description: SharedString::from(t!(
                            "agent-rewind-conversation-description"
                        )),
                        hint: Some(SharedString::from(t!("agent-rewind-conversation-only"))),
                        disabled_reason: None,
                        action: PaletteAction::RewindAction(RewindAction::Conversation),
                    },
                    PaletteRow {
                        label: SharedString::from(t!("agent-rewind-restore-files-conversation")),
                        description: SharedString::from(t!("agent-rewind-combined-description")),
                        hint: Some(SharedString::from(t!("agent-rewind-combined"))),
                        disabled_reason: file_disabled,
                        action: PaletteAction::RewindAction(RewindAction::FilesAndConversation),
                    },
                    rewind_cancel_row(),
                ],
                note: Some(
                    t!(
                        "agent-rewind-selected",
                        prompt = &rewind_prompt_label(&checkpoint.prompt)
                    )
                    .into_owned()
                    .into(),
                ),
            })
        }
        _ => None,
    }
}

fn rewind_cancel_row() -> PaletteRow {
    PaletteRow {
        label: SharedString::from(t!("agent-rewind-cancel")),
        description: SharedString::from(t!("agent-rewind-cancel-description")),
        hint: None,
        disabled_reason: None,
        action: PaletteAction::RewindAction(RewindAction::Cancel),
    }
}
