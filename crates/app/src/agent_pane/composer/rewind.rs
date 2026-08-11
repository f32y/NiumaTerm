use crate::agent_pane::composer::{CommandFeedbackKind, PaletteAction, PaletteModel, PaletteRow};
use crate::agent_pane::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::agent_pane) enum RewindAction {
    Files,
    Conversation,
    FilesAndConversation,
    Cancel,
}

pub(in crate::agent_pane) enum RewindState {
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
    pub(in crate::agent_pane) fn is_picker(&self) -> bool {
        matches!(
            self,
            Self::Loading { .. } | Self::SelectingCheckpoint { .. } | Self::SelectingAction { .. }
        )
    }

    pub(in crate::agent_pane) fn has_operation(&self, operation_id: u64) -> bool {
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
pub(in crate::agent_pane) enum FileRestoreNext {
    Complete,
    ForkConversation,
    RetryAction(String),
}

pub(in crate::agent_pane) fn file_restore_next(
    continue_with_fork: bool,
    result: Result<(), String>,
) -> FileRestoreNext {
    match (continue_with_fork, result) {
        (true, Ok(())) => FileRestoreNext::ForkConversation,
        (false, Ok(())) => FileRestoreNext::Complete,
        (_, Err(message)) => FileRestoreNext::RetryAction(message),
    }
}

pub(in crate::agent_pane) fn rewind_blocks_submission(state: Option<&RewindState>) -> bool {
    state.is_some()
}

pub(in crate::agent_pane) fn rewind_prompt_label(prompt: &str) -> String {
    let line = prompt
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("Untitled prompt")
        .trim();
    let mut label = line.chars().take(72).collect::<String>();
    if line.chars().count() > 72 {
        label.push('…');
    }
    label
}

pub(in crate::agent_pane) fn rewind_timestamp(timestamp: Option<&str>) -> Option<String> {
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
    pub(in crate::agent_pane) fn open_rewind(&mut self, cx: &mut Context<Self>) -> bool {
        if self.status != Status::Idle || self.is_command_busy() {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                "/rewind is available only while Claude is idle.".to_string(),
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
                "Claude has not published a resumable session id yet.".to_string(),
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
            CommandFeedbackKind::Notice,
            "Loading Claude rewind checkpoints…".to_string(),
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
                            "This Claude session has no rewindable human prompts.".to_string(),
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

    pub(in crate::agent_pane) fn cancel_rewind_picker(&mut self, cx: &mut Context<Self>) {
        if self
            .rewind
            .state
            .as_ref()
            .is_some_and(RewindState::is_picker)
        {
            self.rewind.state = None;
            self.palette.selected = 0;
            self.set_command_feedback(
                CommandFeedbackKind::Notice,
                "Rewind cancelled; no files or conversation were changed.".to_string(),
                cx,
            );
        }
    }

    pub(in crate::agent_pane) fn rewind_palette_model(
        &self,
        state: &RewindState,
    ) -> Option<PaletteModel> {
        match state {
            RewindState::Loading { .. } => Some(PaletteModel {
                rows: vec![PaletteRow {
                    label: "Cancel".to_string(),
                    description: "Close rewind without changing anything".to_string(),
                    hint: None,
                    disabled_reason: None,
                    action: PaletteAction::RewindAction(RewindAction::Cancel),
                }],
                note: Some("Loading checkpoints from the active Claude branch…".to_string()),
            }),
            RewindState::SelectingCheckpoint { checkpoints, .. } => {
                let mut rows = checkpoints
                    .iter()
                    .cloned()
                    .map(|checkpoint| PaletteRow {
                        label: rewind_prompt_label(&checkpoint.prompt),
                        description: "Return to immediately before this prompt".to_string(),
                        hint: rewind_timestamp(checkpoint.timestamp.as_deref()),
                        disabled_reason: None,
                        action: PaletteAction::RewindCheckpoint(checkpoint),
                    })
                    .collect::<Vec<_>>();
                rows.push(PaletteRow {
                    label: "Cancel".to_string(),
                    description: "Close rewind without changing anything".to_string(),
                    hint: None,
                    disabled_reason: None,
                    action: PaletteAction::RewindAction(RewindAction::Cancel),
                });

                Some(PaletteModel {
                    rows,
                    note: Some("Choose a prompt · newest first · Esc cancels".to_string()),
                })
            }
            RewindState::SelectingAction { checkpoint, .. } => {
                let file_disabled = match checkpoint.file_restore_availability {
                    sessions::FileRestoreAvailability::Unavailable => Some(
                        "No file checkpoint was recorded for this prompt; conversation rewind is still available."
                            .to_string(),
                    ),
                    _ => None,
                };
                let file_description = match checkpoint.file_restore_availability {
                    sessions::FileRestoreAvailability::Available => {
                        "Restore tracked files; keep this conversation and composer unchanged"
                    }
                    sessions::FileRestoreAvailability::Unknown => {
                        "Ask Claude to restore tracked files; checkpoint availability is unknown"
                    }
                    sessions::FileRestoreAvailability::Unavailable => {
                        "No persisted file snapshot is associated with this prompt"
                    }
                };

                Some(PaletteModel {
                    rows: vec![
                        PaletteRow {
                            label: "Restore files".to_string(),
                            description: file_description.to_string(),
                            hint: Some("files only".to_string()),
                            disabled_reason: file_disabled.clone(),
                            action: PaletteAction::RewindAction(RewindAction::Files),
                        },
                        PaletteRow {
                            label: "Restore conversation".to_string(),
                            description:
                                "Open an independent prefix session and put this prompt in the composer"
                                    .to_string(),
                            hint: Some("conversation only".to_string()),
                            disabled_reason: None,
                            action: PaletteAction::RewindAction(RewindAction::Conversation),
                        },
                        PaletteRow {
                            label: "Restore files and conversation".to_string(),
                            description:
                                "Restore files first, then open the independent prefix session"
                                    .to_string(),
                            hint: Some("combined".to_string()),
                            disabled_reason: file_disabled,
                            action: PaletteAction::RewindAction(
                                RewindAction::FilesAndConversation,
                            ),
                        },
                        PaletteRow {
                            label: "Cancel".to_string(),
                            description: "Close rewind without changing anything".to_string(),
                            hint: None,
                            disabled_reason: None,
                            action: PaletteAction::RewindAction(RewindAction::Cancel),
                        },
                    ],
                    note: Some(format!(
                        "Selected: {} · Esc cancels",
                        rewind_prompt_label(&checkpoint.prompt)
                    )),
                })
            }
            RewindState::RestoringFiles { .. } | RewindState::ForkingConversation { .. } => None,
        }
    }

    pub(in crate::agent_pane) fn activate_rewind_action(
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

    pub(in crate::agent_pane) fn start_file_restore(
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
                    "Claude is not ready to restore files.".to_string(),
                    cx,
                );
                return;
            }
            SlashCommandOutcome::Completed { message } => {
                self.set_command_feedback(
                    CommandFeedbackKind::Error,
                    message
                        .unwrap_or_else(|| "Claude returned an invalid file restore state.".into()),
                    cx,
                );
                return;
            }
        }

        let (completion_tx, completion_rx) = oneshot::channel();
        self.rewind.file_completion = Some(completion_tx);
        self.rewind.state = Some(RewindState::RestoringFiles { operation_id });
        self.set_command_feedback(
            CommandFeedbackKind::Notice,
            if continue_with_fork {
                "Restoring Claude files before creating the conversation fork…".to_string()
            } else {
                "Restoring Claude files…".to_string()
            },
            cx,
        );

        cx.spawn_in(window, async move |this, cx| {
            let result = completion_rx.await.unwrap_or_else(|_| {
                Err("The file restore was cancelled before Claude replied.".to_string())
            });

            let _ = this.update_in(cx, |this, window, cx| {
                let is_current = this.rewind.state.as_ref().is_some_and(|state| {
                    state.has_operation(operation_id)
                        && matches!(state, RewindState::RestoringFiles { .. })
                });
                if !is_current {
                    return;
                }

                match file_restore_next(continue_with_fork, result) {
                    FileRestoreNext::ForkConversation => this.start_conversation_fork(
                        operation_id,
                        checkpoint,
                        true,
                        window,
                        cx,
                    ),
                    FileRestoreNext::Complete => {
                        this.rewind.state = None;
                        this.set_command_feedback(
                            CommandFeedbackKind::Notice,
                            "Files restored. The conversation and session id were not changed."
                                .to_string(),
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
                                format!(
                                    "File restore failed; the conversation was not rewound: {message}"
                                )
                            } else {
                                format!("File restore failed: {message}")
                            },
                            cx,
                        );
                    }
                }
            });
        })
        .detach();
    }

    pub(in crate::agent_pane) fn start_conversation_fork(
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
                    "Files were restored, but Claude no longer exposes the source session id. The original conversation remains in history."
                        .to_string()
                } else {
                    "Claude no longer exposes the source session id.".to_string()
                },
                cx,
            );
            return;
        };

        let cwd = self.cwd.clone();
        let user_message_id = checkpoint.user_message_id.clone();
        self.rewind.state = Some(RewindState::ForkingConversation { operation_id });
        self.set_command_feedback(
            CommandFeedbackKind::Notice,
            "Creating an independent Claude conversation prefix…".to_string(),
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
                                format!(
                                    "Files were restored, but conversation rewind failed: {message}. The original session remains available in history."
                                )
                            } else {
                                format!("Conversation rewind failed: {message}")
                            },
                            cx,
                        );
                    }
                }
            });
        })
        .detach();
    }

    pub(in crate::agent_pane) fn replace_with_conversation_fork(
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
        let started = self.start_session_with_options(
            fork.session_id.map(RecoveryIdentity::ClaudeSession),
            true,
            cx,
        );
        self.set_command_feedback(
            if started {
                CommandFeedbackKind::Notice
            } else {
                CommandFeedbackKind::Error
            },
            if !started && files_restored {
                "Files were restored and the conversation fork was created, but Claude could not start it. The original session remains available in history."
                    .to_string()
            } else if !started {
                "The conversation fork was created, but Claude could not start it. The original session remains available in history."
                    .to_string()
            } else if files_restored {
                "Files restored and conversation rewound. Review the recovered prompt, then send it when ready."
                    .to_string()
            } else {
                "Conversation rewound. Review the recovered prompt, then send it when ready."
                    .to_string()
            },
            cx,
        );
    }
}
