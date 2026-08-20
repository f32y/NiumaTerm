use nmt_i18n::i18n;

use crate::agent::composer::{CommandFeedbackKind, PaletteAction, PaletteModel, PaletteRow};
use crate::agent::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::agent) enum RewindAction {
    Files,
    Conversation,
    FilesAndConversation,
    Cancel,
}

pub(in crate::agent) enum RewindState {
    Loading {
        operation_id: u64,
    },
    SelectingCheckpoint {
        operation_id: u64,
        checkpoints: Vec<sessions::ClaudeCheckpoint>,
    },
    SelectingAction {
        operation_id: u64,
        checkpoint: sessions::ClaudeCheckpoint,
    },
    RestoringFiles {
        operation_id: u64,
    },
    ForkingConversation {
        operation_id: u64,
    },
}

impl RewindState {
    pub(in crate::agent) fn is_picker(&self) -> bool {
        matches!(
            self,
            Self::Loading { .. } | Self::SelectingCheckpoint { .. } | Self::SelectingAction { .. }
        )
    }

    pub(in crate::agent) fn has_operation(&self, operation_id: u64) -> bool {
        match self {
            Self::Loading {
                operation_id: current,
            }
            | Self::SelectingCheckpoint {
                operation_id: current,
                ..
            }
            | Self::SelectingAction {
                operation_id: current,
                ..
            }
            | Self::RestoringFiles {
                operation_id: current,
            }
            | Self::ForkingConversation {
                operation_id: current,
            } => *current == operation_id,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(in crate::agent) enum FileRestoreNext {
    Complete,
    ForkConversation,
    RetryAction(String),
}

pub(in crate::agent) fn file_restore_next(
    continue_with_fork: bool,
    result: Result<(), String>,
) -> FileRestoreNext {
    match (continue_with_fork, result) {
        (true, Ok(())) => FileRestoreNext::ForkConversation,
        (false, Ok(())) => FileRestoreNext::Complete,
        (_, Err(message)) => FileRestoreNext::RetryAction(message),
    }
}

pub(in crate::agent) fn rewind_blocks_submission(state: Option<&RewindState>) -> bool {
    state.is_some()
}

pub(in crate::agent) fn rewind_prompt_label(prompt: &str) -> String {
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

pub(in crate::agent) fn rewind_timestamp(timestamp: Option<&str>) -> Option<String> {
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
    pub(in crate::agent) fn open_rewind(&mut self, cx: &mut Context<Self>) -> bool {
        if self.status != Status::Idle || self.is_command_busy() {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-rewind-idle-only").to_string(),
                cx,
            );
            return false;
        }

        let Some(session_id) = self
            .session
            .as_ref()
            .and_then(Backend::session_id)
            .map(str::to_owned)
        else {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-rewind-no-session-id").to_string(),
                cx,
            );
            return false;
        };

        self.rewind.operation_seq = self.rewind.operation_seq.wrapping_add(1);
        let operation_id = self.rewind.operation_seq;
        self.rewind.state = Some(RewindState::Loading { operation_id });
        self.palette.selected = 0;
        self.palette.dismissed = false;
        self.set_command_feedback(
            CommandFeedbackKind::Status,
            i18n("agent-rewind-loading-checkpoints").to_string(),
            cx,
        );

        let cwd = self.cwd.clone();
        let load = cx
            .background_executor()
            .spawn(async move { sessions::load_checkpoints(cwd.as_deref(), &session_id) });
        cx.spawn(async move |this, cx| {
            let checkpoints = load.await;

            let _ = this.update(cx, |this, cx| {
                let is_current = matches!(
                    &this.rewind.state,
                    Some(RewindState::Loading {
                        operation_id: current,
                    }) if *current == operation_id
                );
                if !is_current {
                    return;
                }

                match checkpoints {
                    Ok(checkpoints) if checkpoints.is_empty() => {
                        this.rewind.state = None;
                        this.set_command_feedback(
                            CommandFeedbackKind::Error,
                            i18n("agent-rewind-no-prompts").to_string(),
                            cx,
                        );
                    }
                    Ok(checkpoints) => {
                        this.rewind.state = Some(RewindState::SelectingCheckpoint {
                            operation_id,
                            checkpoints,
                        });
                        this.palette.feedback = None;
                        this.palette.selected = 0;
                        cx.notify();
                    }
                    Err(message) => {
                        this.rewind.state = None;
                        this.set_command_feedback(CommandFeedbackKind::Error, message, cx);
                    }
                }
            });
        })
        .detach();

        true
    }

    pub(in crate::agent) fn cancel_rewind_picker(&mut self, cx: &mut Context<Self>) {
        if self
            .rewind
            .state
            .as_ref()
            .is_some_and(RewindState::is_picker)
        {
            self.rewind.state = None;
            self.palette.selected = 0;
            // Cancelling is the user's own no-op, so an acknowledgement tells
            // them nothing they do not already know. Dropping the message also
            // retires the non-transient "Loading checkpoints…" status, which
            // otherwise outlives the picker it described.
            self.palette.feedback = None;
            cx.notify();
        }
    }

    pub(in crate::agent) fn rewind_palette_model(
        &self,
        state: &RewindState,
    ) -> Option<PaletteModel> {
        match state {
            RewindState::Loading { .. } => Some(PaletteModel {
                rows: vec![PaletteRow {
                    label: i18n("agent-rewind-cancel").to_string(),
                    description: i18n("agent-rewind-cancel-description").to_string(),
                    hint: None,
                    disabled_reason: None,
                    action: PaletteAction::RewindAction(RewindAction::Cancel),
                }],
                note: Some(i18n("agent-rewind-loading-active-branch").to_string()),
            }),
            RewindState::SelectingCheckpoint { checkpoints, .. } => {
                let mut rows = checkpoints
                    .iter()
                    .cloned()
                    .map(|checkpoint| PaletteRow {
                        label: rewind_prompt_label(&checkpoint.prompt),
                        description: i18n("agent-rewind-return-before-prompt").to_string(),
                        hint: rewind_timestamp(checkpoint.timestamp.as_deref()),
                        disabled_reason: None,
                        action: PaletteAction::RewindCheckpoint(checkpoint),
                    })
                    .collect::<Vec<_>>();
                rows.push(PaletteRow {
                    label: i18n("agent-rewind-cancel").to_string(),
                    description: i18n("agent-rewind-cancel-description").to_string(),
                    hint: None,
                    disabled_reason: None,
                    action: PaletteAction::RewindAction(RewindAction::Cancel),
                });

                Some(PaletteModel {
                    rows,
                    note: Some(i18n("agent-rewind-choose-prompt").to_string()),
                })
            }
            RewindState::SelectingAction { checkpoint, .. } => {
                let file_disabled = match checkpoint.file_restore_availability {
                    sessions::FileRestoreAvailability::Unavailable => {
                        Some(i18n("agent-rewind-file-checkpoint-unavailable").to_string())
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

                Some(PaletteModel {
                    rows: vec![
                        PaletteRow {
                            label: i18n("agent-rewind-restore-files").to_string(),
                            description: file_description.to_string(),
                            hint: Some(i18n("agent-rewind-files-only").to_string()),
                            disabled_reason: file_disabled.clone(),
                            action: PaletteAction::RewindAction(RewindAction::Files),
                        },
                        PaletteRow {
                            label: i18n("agent-rewind-restore-conversation").to_string(),
                            description: i18n("agent-rewind-conversation-description").to_string(),
                            hint: Some(i18n("agent-rewind-conversation-only").to_string()),
                            disabled_reason: None,
                            action: PaletteAction::RewindAction(RewindAction::Conversation),
                        },
                        PaletteRow {
                            label: i18n("agent-rewind-restore-files-conversation").to_string(),
                            description: i18n("agent-rewind-combined-description").to_string(),
                            hint: Some(i18n("agent-rewind-combined").to_string()),
                            disabled_reason: file_disabled,
                            action: PaletteAction::RewindAction(RewindAction::FilesAndConversation),
                        },
                        PaletteRow {
                            label: i18n("agent-rewind-cancel").to_string(),
                            description: i18n("agent-rewind-cancel-description").to_string(),
                            hint: None,
                            disabled_reason: None,
                            action: PaletteAction::RewindAction(RewindAction::Cancel),
                        },
                    ],
                    note: Some(
                        i18n("agent-rewind-selected")
                            .replace("{prompt}", &rewind_prompt_label(&checkpoint.prompt)),
                    ),
                })
            }
            RewindState::RestoringFiles { .. } | RewindState::ForkingConversation { .. } => None,
        }
    }

    pub(in crate::agent) fn activate_rewind_action(
        &mut self,
        action: RewindAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if action == RewindAction::Cancel {
            self.cancel_rewind_picker(cx);
            return;
        }

        let Some((operation_id, checkpoint)) =
            self.rewind.state.as_ref().and_then(|state| match state {
                RewindState::SelectingAction {
                    operation_id,
                    checkpoint,
                } => Some((*operation_id, checkpoint.clone())),
                _ => None,
            })
        else {
            return;
        };

        match action {
            RewindAction::Files => {
                self.start_file_restore(operation_id, checkpoint, false, window, cx)
            }
            RewindAction::Conversation => {
                self.start_conversation_fork(operation_id, checkpoint, false, window, cx)
            }
            RewindAction::FilesAndConversation => {
                self.start_file_restore(operation_id, checkpoint, true, window, cx)
            }
            RewindAction::Cancel => unreachable!(),
        }
    }

    pub(in crate::agent) fn start_file_restore(
        &mut self,
        operation_id: u64,
        checkpoint: sessions::ClaudeCheckpoint,
        continue_with_fork: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let outcome = self
            .session
            .as_mut()
            .map(|session| session.rewind_files(&checkpoint.user_message_id))
            .unwrap_or(SlashCommandOutcome::NotReady);

        match outcome {
            SlashCommandOutcome::Accepted => {}
            SlashCommandOutcome::Rejected { message } => {
                self.set_command_feedback(CommandFeedbackKind::Error, message, cx);
                return;
            }
            SlashCommandOutcome::NotReady => {
                self.set_command_feedback(
                    CommandFeedbackKind::Error,
                    i18n("agent-rewind-files-not-ready").to_string(),
                    cx,
                );
                return;
            }
            SlashCommandOutcome::Completed { message } => {
                self.set_command_feedback(
                    CommandFeedbackKind::Error,
                    message.unwrap_or_else(|| i18n("agent-rewind-invalid-file-state").to_string()),
                    cx,
                );
                return;
            }
        }

        let (completion_tx, completion_rx) = oneshot::channel();
        self.rewind.file_completion = Some(completion_tx);
        self.rewind.state = Some(RewindState::RestoringFiles { operation_id });
        self.set_command_feedback(
            CommandFeedbackKind::Status,
            if continue_with_fork {
                i18n("agent-rewind-restoring-before-fork").to_string()
            } else {
                i18n("agent-rewind-restoring-files").to_string()
            },
            cx,
        );

        cx.spawn_in(window, async move |this, cx| {
            let result = completion_rx
                .await
                .unwrap_or_else(|_| Err(i18n("agent-rewind-file-restore-cancelled").to_string()));

            let _ = this.update_in(cx, |this, window, cx| {
                let is_current = this.rewind.state.as_ref().is_some_and(|state| {
                    state.has_operation(operation_id)
                        && matches!(state, RewindState::RestoringFiles { .. })
                });
                if !is_current {
                    return;
                }

                match file_restore_next(continue_with_fork, result) {
                    FileRestoreNext::ForkConversation => {
                        this.start_conversation_fork(operation_id, checkpoint, true, window, cx)
                    }
                    FileRestoreNext::Complete => {
                        this.rewind.state = None;
                        this.set_command_feedback(
                            CommandFeedbackKind::Notice,
                            i18n("agent-rewind-files-restored").to_string(),
                            cx,
                        );
                    }
                    FileRestoreNext::RetryAction(message) => {
                        this.rewind.state = Some(RewindState::SelectingAction {
                            operation_id,
                            checkpoint,
                        });
                        this.palette.selected = 0;
                        this.set_command_feedback(
                            CommandFeedbackKind::Error,
                            if continue_with_fork {
                                i18n("agent-rewind-file-failed-no-conversation")
                                    .replace("{error}", &message)
                            } else {
                                i18n("agent-rewind-file-failed").replace("{error}", &message)
                            },
                            cx,
                        );
                    }
                }
            });
        })
        .detach();
    }

    pub(in crate::agent) fn start_conversation_fork(
        &mut self,
        operation_id: u64,
        checkpoint: sessions::ClaudeCheckpoint,
        files_restored: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(source_session_id) = self
            .session
            .as_ref()
            .and_then(Backend::session_id)
            .map(str::to_owned)
        else {
            self.rewind.state = Some(RewindState::SelectingAction {
                operation_id,
                checkpoint,
            });
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                if files_restored {
                    i18n("agent-rewind-source-id-missing-after-files").to_string()
                } else {
                    i18n("agent-rewind-source-id-missing").to_string()
                },
                cx,
            );
            return;
        };

        let cwd = self.cwd.clone();
        let user_message_id = checkpoint.user_message_id.clone();
        self.rewind.state = Some(RewindState::ForkingConversation { operation_id });
        self.set_command_feedback(
            CommandFeedbackKind::Status,
            i18n("agent-rewind-creating-prefix").to_string(),
            cx,
        );

        let fork = cx.background_executor().spawn(async move {
            sessions::fork_session_before(cwd.as_deref(), &source_session_id, &user_message_id)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = fork.await;

            let _ = this.update_in(cx, |this, window, cx| {
                let is_current = this.rewind.state.as_ref().is_some_and(|state| {
                    state.has_operation(operation_id)
                        && matches!(state, RewindState::ForkingConversation { .. })
                });
                if !is_current {
                    return;
                }

                match result {
                    Ok(fork) => this.replace_with_conversation_fork(
                        fork,
                        checkpoint.prompt,
                        files_restored,
                        window,
                        cx,
                    ),
                    Err(message) => {
                        this.rewind.state = None;
                        this.set_command_feedback(
                            CommandFeedbackKind::Error,
                            if files_restored {
                                i18n("agent-rewind-conversation-failed-after-files")
                                    .replace("{error}", &message)
                            } else {
                                i18n("agent-rewind-conversation-failed")
                                    .replace("{error}", &message)
                            },
                            cx,
                        );
                    }
                }
            });
        })
        .detach();
    }

    pub(in crate::agent) fn replace_with_conversation_fork(
        &mut self,
        fork: sessions::ClaudeFork,
        prompt: String,
        files_restored: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.session = None;
        self.clear_conversation_presentation(cx);
        self.palette.skill_catalog = None;
        self.palette.skill_binding = None;
        reset_command_runtime(
            false,
            &mut self.pending_approval,
            &mut self.palette.provider_commands,
            &mut self.palette.provider_commands_ready,
            &mut self.palette.command_queue,
            &mut self.palette.awaiting_command_turn,
            &mut self.palette.selected,
            &mut self.palette.dismissed,
        );
        self.history_ui.mode = RecentSessionsMode::Hidden;

        self.apply_replay(fork.replay, cx);
        self.input.update(cx, |input, cx| {
            input.set_value(prompt.clone(), window, cx);
            input.set_selected_range(prompt.len()..prompt.len(), cx);
        });
        self.start_session_with_options(
            fork.session_id
                .map(|id| RecoveryIdentity::new(AgentKind::Claude, id)),
            true,
            move |this, started, cx| {
                this.set_command_feedback(
                    if started {
                        CommandFeedbackKind::Notice
                    } else {
                        CommandFeedbackKind::Error
                    },
                    if !started && files_restored {
                        i18n("agent-rewind-start-failed-after-files").to_string()
                    } else if !started {
                        i18n("agent-rewind-start-failed").to_string()
                    } else if files_restored {
                        i18n("agent-rewind-complete-with-files").to_string()
                    } else {
                        i18n("agent-rewind-complete").to_string()
                    },
                    cx,
                );
            },
            cx,
        );
    }
}
