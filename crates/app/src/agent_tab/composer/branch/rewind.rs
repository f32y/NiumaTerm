pub(crate) use nmt_agent::session::branch::RewindAction;

use std::borrow::Cow;

use chrono::Local;
use gpui::{Context, SharedString};
use nmt_agent::claude_code::sessions;
use nmt_agent::session::branch::{
    BranchError, BranchFailure, BranchUpdate, BranchView, FailureStage, FileProgress, PromptTarget,
};
use rust_i18n::t;

use crate::agent_tab::composer::{CommandFeedbackKind, PaletteAction, PaletteModel, PaletteRow};
use crate::agent_tab::session::Status;
use crate::agent_tab::session::errors::operation_error;
use crate::agent_tab::{AgentPane, RecentSessionsMode};

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

impl AgentPane {
    pub(crate) fn rewind_to_prompt(
        &mut self,
        target: PromptTarget,
        cx: &mut Context<Self>,
    ) -> bool {
        self.load_rewind_checkpoints(Some(target), cx)
    }

    pub(crate) fn open_rewind(&mut self, cx: &mut Context<Self>) -> bool {
        self.load_rewind_checkpoints(None, cx)
    }

    fn load_rewind_checkpoints(
        &mut self,
        target: Option<PromptTarget>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.binding.is_current() {
            return false;
        }

        if self.session.borrow().runtime.status() != Status::Idle || self.is_command_busy() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                SharedString::from(t!("agent-rewind-idle-only")),
                cx,
            );

            return false;
        }

        let cwd = self.cwd();

        let outcome = {
            let mut guard = self.session.borrow_mut();
            let state = &mut *guard;

            state.branch.begin_rewind(&state.runtime, cwd, target)
        };

        let request = match outcome {
            Ok(request) => request,

            Err(error) => {
                let message = self.branch_error_message(error);

                self.palette
                    .set_feedback(CommandFeedbackKind::Error, message, cx);

                return false;
            }
        };

        self.branch.pending_prompt = None;
        self.palette.selected = 0;
        self.palette.dismissed = false;

        self.palette.set_feedback(
            CommandFeedbackKind::Status,
            SharedString::from(t!("agent-rewind-loading-checkpoints")),
            cx,
        );

        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, cx| host.read_checkpoints(request, cx));
        }

        true
    }

    pub(crate) fn cancel_rewind_picker(&mut self, cx: &mut Context<Self>) {
        self.cancel_branch_picker(cx);
    }

    pub(crate) fn rewind_palette_model(&self, state: BranchView<'_>) -> Option<PaletteModel> {
        match state {
            BranchView::LoadingRewind => Some(PaletteModel {
                rows: vec![PaletteRow {
                    label: SharedString::from(t!("agent-rewind-cancel")),
                    description: SharedString::from(t!("agent-rewind-cancel-description")),
                    hint: None,
                    disabled_reason: None,
                    action: PaletteAction::RewindAction(RewindAction::Cancel),
                }],
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

                rows.push(PaletteRow {
                    label: SharedString::from(t!("agent-rewind-cancel")),
                    description: SharedString::from(t!("agent-rewind-cancel-description")),
                    hint: None,
                    disabled_reason: None,
                    action: PaletteAction::RewindAction(RewindAction::Cancel),
                });

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
                            label: SharedString::from(t!(
                                "agent-rewind-restore-files-conversation"
                            )),
                            description: SharedString::from(t!(
                                "agent-rewind-combined-description"
                            )),
                            hint: Some(SharedString::from(t!("agent-rewind-combined"))),
                            disabled_reason: file_disabled,
                            action: PaletteAction::RewindAction(RewindAction::FilesAndConversation),
                        },
                        PaletteRow {
                            label: SharedString::from(t!("agent-rewind-cancel")),
                            description: SharedString::from(t!("agent-rewind-cancel-description")),
                            hint: None,
                            disabled_reason: None,
                            action: PaletteAction::RewindAction(RewindAction::Cancel),
                        },
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

    pub(crate) fn activate_rewind_action(&mut self, action: RewindAction, cx: &mut Context<Self>) {
        if !self.binding.is_current() {
            return;
        }

        if action == RewindAction::Cancel {
            self.cancel_rewind_picker(cx);

            return;
        }

        self.branch.draft = Some(self.input.read(cx).text().to_string());

        let update = {
            let mut guard = self.session.borrow_mut();
            let state = &mut *guard;

            state.branch.rewind(&mut state.runtime, action)
        };

        self.on_rewind_update(update, cx);
    }

    pub(crate) fn on_rewind_update(&mut self, update: BranchUpdate, cx: &mut Context<Self>) {
        match update {
            BranchUpdate::Ignored => {}

            BranchUpdate::Empty => self.palette.set_feedback(
                CommandFeedbackKind::Error,
                SharedString::from(t!("agent-rewind-no-prompts")),
                cx,
            ),

            BranchUpdate::Picker { unresolved } => {
                self.palette.selected = 0;
                self.palette.feedback = None;

                if unresolved {
                    self.palette.set_feedback(
                        CommandFeedbackKind::Error,
                        SharedString::from(t!("agent-rewind-prompt-not-a-checkpoint")),
                        cx,
                    );
                }

                self.hold_transcript_for_picker(cx);
                self.follow_branch_selection(cx);

                cx.notify();
            }

            BranchUpdate::RestoringFiles(action) => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Status,
                    match action {
                        RewindAction::FilesAndConversation => {
                            SharedString::from(t!("agent-rewind-restoring-before-fork"))
                        }

                        _ => SharedString::from(t!("agent-rewind-restoring-files")),
                    },
                    cx,
                );
            }

            update @ (BranchUpdate::CreateFork(_) | BranchUpdate::StartSession(_)) => {
                self.history_ui.mode = RecentSessionsMode::Loading;
                self.palette.reset_discovery(false);

                if let Some(host) = self.host.upgrade() {
                    host.update(cx, |host, cx| host.on_branch_update(update, cx));
                }
            }

            BranchUpdate::FilesRestored => {
                self.branch.draft = None;
                self.release_transcript_from_picker(cx);

                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    SharedString::from(t!("agent-rewind-files-restored")),
                    cx,
                );
            }

            BranchUpdate::Failed(failure) => self.report_branch_failure(failure, cx),
            BranchUpdate::Branching => {}
        }
    }

    pub(crate) fn branch_error_message(&self, error: BranchError) -> String {
        match error {
            BranchError::Busy => t!("agent-rewind-idle-only").to_string(),

            BranchError::NotReady => {
                t!("agent-session-still-starting", name = self.kind.display()).into_owned()
            }

            BranchError::MissingSession => t!("agent-rewind-no-session-id").to_string(),

            BranchError::FilesUnavailable => {
                t!("agent-rewind-file-checkpoint-unavailable").to_string()
            }

            BranchError::InvalidFileResult(message) => {
                message.unwrap_or_else(|| t!("agent-rewind-invalid-file-state").to_string())
            }

            BranchError::Operation(error) => operation_error(error),
            BranchError::Failed(message) => message,
        }
    }

    pub(crate) fn report_branch_failure(&mut self, failure: BranchFailure, cx: &mut Context<Self>) {
        let error = self.branch_error_message(failure.error);

        let message = match (failure.stage, failure.files) {
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
        };

        self.palette.selected = 0;

        self.palette
            .set_feedback(CommandFeedbackKind::Error, message, cx);
    }
}
