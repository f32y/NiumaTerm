use chrono::Local;
use gpui::{Context, SharedString};
use nmt_agent::claude_code::sessions;
pub(crate) use nmt_agent::session::branch::RewindAction;
use nmt_agent::session::branch::{
    BranchError, BranchFailure, BranchUpdate, BranchView, FailureStage, FileProgress, PromptTarget,
};
use nmt_i18n::i18n;

use crate::composer::{CommandFeedbackKind, PaletteAction, PaletteModel, PaletteRow};
use crate::session::Status;
use crate::session::errors::operation_error;
use crate::{AgentPane, RecentSessionsMode, translated};

pub(crate) fn rewind_prompt_label(prompt: &str) -> String {
    let line = prompt
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_else(|| i18n("agent-rewind-untitled-prompt"))
        .trim();

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
        if self.runtime.status() != Status::Idle || self.is_command_busy() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                translated("agent-rewind-idle-only"),
                cx,
            );
            return false;
        }
        let cwd = self.cwd();
        let request = match self.branch.core.begin_rewind(&self.runtime, cwd, target) {
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
            translated("agent-rewind-loading-checkpoints"),
            cx,
        );
        cx.spawn(async move |this, cx| {
            let (request, result) = cx
                .background_executor()
                .spawn(async move {
                    let result = request.load();
                    (request, result)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let update =
                    this.branch
                        .core
                        .checkpoints_loaded(this.runtime.epoch(), request, result);
                this.apply_rewind_update(update, cx);
            });
        })
        .detach();
        true
    }

    pub(crate) fn cancel_rewind_picker(&mut self, cx: &mut Context<Self>) {
        self.cancel_branch_picker(cx);
    }

    pub(crate) fn rewind_palette_model(&self, state: BranchView<'_>) -> Option<PaletteModel> {
        match state {
            BranchView::LoadingRewind => Some(PaletteModel {
                rows: vec![PaletteRow {
                    label: translated("agent-rewind-cancel"),
                    description: translated("agent-rewind-cancel-description"),
                    hint: None,
                    disabled_reason: None,
                    action: PaletteAction::RewindAction(RewindAction::Cancel),
                }],
                note: Some(translated("agent-rewind-loading-active-branch")),
            }),
            BranchView::RewindCheckpoints(checkpoints) => {
                let mut rows = checkpoints
                    .iter()
                    .cloned()
                    .map(|checkpoint| PaletteRow {
                        label: rewind_prompt_label(&checkpoint.prompt).into(),
                        description: translated("agent-rewind-return-before-prompt"),
                        hint: rewind_timestamp(checkpoint.timestamp.as_deref())
                            .map(SharedString::from),
                        disabled_reason: None,
                        action: PaletteAction::RewindCheckpoint(checkpoint),
                    })
                    .collect::<Vec<_>>();

                rows.push(PaletteRow {
                    label: translated("agent-rewind-cancel"),
                    description: translated("agent-rewind-cancel-description"),
                    hint: None,
                    disabled_reason: None,
                    action: PaletteAction::RewindAction(RewindAction::Cancel),
                });

                Some(PaletteModel {
                    rows,
                    note: Some(translated("agent-rewind-choose-prompt")),
                })
            }
            BranchView::RewindAction(checkpoint, files) => {
                let file_disabled = match checkpoint.file_restore_availability {
                    sessions::FileRestoreAvailability::Unavailable => {
                        Some(translated("agent-rewind-file-checkpoint-unavailable"))
                    }
                    _ => None,
                };

                let file_description = match checkpoint.file_restore_availability {
                    sessions::FileRestoreAvailability::Available => {
                        i18n("agent-rewind-files-description-available")
                    }
                    sessions::FileRestoreAvailability::Unknown => {
                        i18n("agent-rewind-files-description-unknown")
                    }
                    sessions::FileRestoreAvailability::Unavailable => {
                        i18n("agent-rewind-files-description-unavailable")
                    }
                };
                let files_only_disabled = match files {
                    FileProgress::Restored => Some(translated("agent-rewind-files-restored")),
                    FileProgress::NotConfirmed => file_disabled.clone(),
                };

                Some(PaletteModel {
                    rows: vec![
                        PaletteRow {
                            label: translated("agent-rewind-restore-files"),
                            description: SharedString::new_static(file_description),
                            hint: Some(translated("agent-rewind-files-only")),
                            disabled_reason: files_only_disabled,
                            action: PaletteAction::RewindAction(RewindAction::Files),
                        },
                        PaletteRow {
                            label: translated("agent-rewind-restore-conversation"),
                            description: translated("agent-rewind-conversation-description"),
                            hint: Some(translated("agent-rewind-conversation-only")),
                            disabled_reason: None,
                            action: PaletteAction::RewindAction(RewindAction::Conversation),
                        },
                        PaletteRow {
                            label: translated("agent-rewind-restore-files-conversation"),
                            description: translated("agent-rewind-combined-description"),
                            hint: Some(translated("agent-rewind-combined")),
                            disabled_reason: file_disabled,
                            action: PaletteAction::RewindAction(RewindAction::FilesAndConversation),
                        },
                        PaletteRow {
                            label: translated("agent-rewind-cancel"),
                            description: translated("agent-rewind-cancel-description"),
                            hint: None,
                            disabled_reason: None,
                            action: PaletteAction::RewindAction(RewindAction::Cancel),
                        },
                    ],
                    note: Some(
                        i18n("agent-rewind-selected")
                            .replace("{prompt}", &rewind_prompt_label(&checkpoint.prompt))
                            .into(),
                    ),
                })
            }
            _ => None,
        }
    }

    pub(crate) fn activate_rewind_action(&mut self, action: RewindAction, cx: &mut Context<Self>) {
        if action == RewindAction::Cancel {
            self.cancel_rewind_picker(cx);
            return;
        }
        self.branch.draft = Some(self.input.read(cx).text().to_string());
        let update = self.branch.core.rewind(&mut self.runtime, action);
        self.apply_rewind_update(update, cx);
    }

    pub(crate) fn apply_rewind_update(&mut self, update: BranchUpdate, cx: &mut Context<Self>) {
        match update {
            BranchUpdate::Ignored => {}
            BranchUpdate::Empty => self.palette.set_feedback(
                CommandFeedbackKind::Error,
                translated("agent-rewind-no-prompts"),
                cx,
            ),
            BranchUpdate::Picker { unresolved } => {
                self.palette.selected = 0;
                self.palette.feedback = None;
                if unresolved {
                    self.palette.set_feedback(
                        CommandFeedbackKind::Error,
                        translated("agent-rewind-prompt-not-a-checkpoint"),
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
                            translated("agent-rewind-restoring-before-fork")
                        }
                        _ => translated("agent-rewind-restoring-files"),
                    },
                    cx,
                );
            }
            BranchUpdate::CreateFork(request) => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Status,
                    translated("agent-rewind-creating-prefix"),
                    cx,
                );
                cx.spawn(async move |this, cx| {
                    let (request, result) = cx
                        .background_executor()
                        .spawn(async move {
                            let result = request.run();
                            (request, result)
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        let update =
                            this.branch
                                .core
                                .fork_created(this.runtime.epoch(), request, result);
                        this.apply_rewind_update(update, cx);
                    });
                })
                .detach();
            }
            BranchUpdate::StartSession(identity) => {
                self.palette.reset_command_runtime(false);
                self.history_ui.mode = RecentSessionsMode::Loading;
                self.start_session_with_options(
                    identity,
                    true,
                    |this, started, cx| {
                        if !started {
                            let error = this
                                .runtime
                                .start_failure()
                                .unwrap_or("session did not start")
                                .to_owned();
                            if let Some(failure) = this.branch.core.failed(&mut this.runtime, error)
                            {
                                this.history_ui.mode = RecentSessionsMode::Open;
                                this.report_branch_failure(failure, cx);
                            }
                        }
                    },
                    cx,
                );
            }
            BranchUpdate::FilesRestored => {
                self.branch.draft = None;
                self.release_transcript_from_picker(cx);
                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    translated("agent-rewind-files-restored"),
                    cx,
                );
            }
            BranchUpdate::Failed(failure) => self.report_branch_failure(failure, cx),
            BranchUpdate::Branching => {}
        }
    }

    pub(crate) fn branch_error_message(&self, error: BranchError) -> String {
        match error {
            BranchError::Busy => i18n("agent-rewind-idle-only").to_string(),
            BranchError::NotReady => {
                i18n("agent-session-still-starting").replace("{name}", self.kind.display())
            }
            BranchError::MissingSession => i18n("agent-rewind-no-session-id").to_string(),
            BranchError::FilesUnavailable => {
                i18n("agent-rewind-file-checkpoint-unavailable").to_string()
            }
            BranchError::InvalidFileResult(message) => {
                message.unwrap_or_else(|| i18n("agent-rewind-invalid-file-state").to_string())
            }
            BranchError::Operation(error) => operation_error(error),
            BranchError::Failed(message) => message,
        }
    }

    pub(crate) fn report_branch_failure(&mut self, failure: BranchFailure, cx: &mut Context<Self>) {
        let error = self.branch_error_message(failure.error);
        let message = match (failure.stage, failure.files) {
            (FailureStage::Checkpoints | FailureStage::ProtocolFork, _) => error,
            (FailureStage::Files, _) => i18n("agent-rewind-file-failed").replace("{error}", &error),
            (FailureStage::Conversation, FileProgress::Restored) => {
                i18n("agent-rewind-conversation-failed-after-files").replace("{error}", &error)
            }
            (FailureStage::Conversation, FileProgress::NotConfirmed) => {
                i18n("agent-rewind-conversation-failed").replace("{error}", &error)
            }
            (FailureStage::Startup, FileProgress::Restored) => {
                i18n("agent-rewind-start-failed-after-files").to_string()
            }
            (FailureStage::Startup, FileProgress::NotConfirmed) => {
                i18n("agent-rewind-start-failed").to_string()
            }
        };
        self.palette.selected = 0;
        self.palette
            .set_feedback(CommandFeedbackKind::Error, message, cx);
    }
}
