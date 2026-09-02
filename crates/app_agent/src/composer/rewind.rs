use chrono::Local;
use futures::channel::oneshot;
use gpui::{Context, SharedString, Window};
use nmt_agent_utils::chat::SlashCommandOutcome;
use nmt_agent_utils::claude_code::sessions;
use nmt_i18n::i18n;

use crate::commands::reset_command_runtime;
use crate::composer::fork::{PromptTarget, checkpoint_at_depth};
use crate::composer::{CommandFeedbackKind, PaletteAction, PaletteModel, PaletteRow};
use crate::profile::AgentKind;
use crate::session::{Backend, RecoveryIdentity, Status};
use crate::{AgentPane, RecentSessionsMode, translated};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RewindAction {
    Files,
    Conversation,
    FilesAndConversation,
    Cancel,
}

pub(crate) enum RewindState {
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
    pub(crate) fn is_picker(&self) -> bool {
        matches!(
            self,
            Self::Loading { .. } | Self::SelectingCheckpoint { .. } | Self::SelectingAction { .. }
        )
    }

    pub(crate) fn has_operation(&self, operation_id: u64) -> bool {
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
pub(crate) enum FileRestoreNext {
    Complete,
    ForkConversation,
    RetryAction(String),
}

pub(crate) fn file_restore_next(
    continue_with_fork: bool,
    result: Result<(), String>,
) -> FileRestoreNext {
    match (continue_with_fork, result) {
        (true, Ok(())) => FileRestoreNext::ForkConversation,
        (false, Ok(())) => FileRestoreNext::Complete,
        (_, Err(message)) => FileRestoreNext::RetryAction(message),
    }
}

pub(crate) fn rewind_blocks_submission(state: Option<&RewindState>) -> bool {
    state.is_some()
}

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
    /// Rewind to one prompt the user pointed at in the transcript.
    ///
    /// The checkpoints still have to be read — only the transcript file
    /// records what a rewind can be anchored on — so this is the same load the
    /// picker does, carrying which of its answers to act on. What is skipped is
    /// only choosing the prompt: the choice between restoring files, the
    /// conversation, or both is what "rewind" leaves open, and it is still
    /// offered.
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
        if self.runtime.status != Status::Idle || self.is_command_busy() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                translated("agent-rewind-idle-only"),
                cx,
            );
            return false;
        }

        let Some(session_id) = self
            .runtime
            .backend
            .as_ref()
            .and_then(Backend::session_id)
            .map(str::to_owned)
        else {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                translated("agent-rewind-no-session-id"),
                cx,
            );
            return false;
        };

        self.rewind.operation_seq = self.rewind.operation_seq.wrapping_add(1);
        let operation_id = self.rewind.operation_seq;
        self.rewind.state = Some(RewindState::Loading { operation_id });
        self.palette.selected = 0;
        self.palette.dismissed = false;
        self.palette.set_feedback(
            CommandFeedbackKind::Status,
            translated("agent-rewind-loading-checkpoints"),
            cx,
        );

        let cwd = self.cwd();
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
                        this.palette.set_feedback(
                            CommandFeedbackKind::Error,
                            translated("agent-rewind-no-prompts"),
                            cx,
                        );
                    }
                    Ok(checkpoints) => {
                        // A pointed-at prompt skips ahead to the actions for
                        // it; one the checkpoints do not confirm falls back to
                        // the list, where the user can see why.
                        let pointed_at = target.as_ref().and_then(|target| {
                            checkpoint_at_depth(&checkpoints, target, |checkpoint| {
                                checkpoint.prompt.as_str()
                            })
                            .cloned()
                        });
                        let unresolved = target.is_some() && pointed_at.is_none();
                        this.rewind.state = Some(match pointed_at {
                            Some(checkpoint) => RewindState::SelectingAction {
                                operation_id,
                                checkpoint,
                            },
                            None => RewindState::SelectingCheckpoint {
                                operation_id,
                                checkpoints,
                            },
                        });
                        if unresolved {
                            this.palette.set_feedback(
                                CommandFeedbackKind::Error,
                                translated("agent-rewind-prompt-not-a-checkpoint"),
                                cx,
                            );
                        } else {
                            this.palette.feedback = None;
                        }
                        this.palette.selected = 0;
                        this.hold_transcript_for_picker(cx);
                        // The newest prompt is highlighted, and it usually
                        // sits under the picker that just opened over the
                        // bottom of the transcript.
                        this.follow_branch_selection(cx);
                        cx.notify();
                    }
                    Err(message) => {
                        this.rewind.state = None;
                        this.palette
                            .set_feedback(CommandFeedbackKind::Error, message, cx);
                    }
                }
            });
        })
        .detach();

        true
    }

    pub(crate) fn cancel_rewind_picker(&mut self, cx: &mut Context<Self>) {
        if self
            .rewind
            .state
            .as_ref()
            .is_some_and(RewindState::is_picker)
        {
            self.rewind.state = None;
            self.palette.selected = 0;
            self.release_transcript_from_picker(cx);
            // Cancelling is the user's own no-op, so an acknowledgement tells
            // them nothing they do not already know. Dropping the message also
            // retires the non-transient "Loading checkpoints…" status, which
            // otherwise outlives the picker it described.
            self.palette.feedback = None;
            cx.notify();
        }
    }

    pub(crate) fn rewind_palette_model(&self, state: &RewindState) -> Option<PaletteModel> {
        match state {
            RewindState::Loading { .. } => Some(PaletteModel {
                rows: vec![PaletteRow {
                    label: translated("agent-rewind-cancel"),
                    description: translated("agent-rewind-cancel-description"),
                    hint: None,
                    disabled_reason: None,
                    action: PaletteAction::RewindAction(RewindAction::Cancel),
                }],
                note: Some(translated("agent-rewind-loading-active-branch")),
            }),
            RewindState::SelectingCheckpoint { checkpoints, .. } => {
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
            RewindState::SelectingAction { checkpoint, .. } => {
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

                Some(PaletteModel {
                    rows: vec![
                        PaletteRow {
                            label: translated("agent-rewind-restore-files"),
                            description: SharedString::new_static(file_description),
                            hint: Some(translated("agent-rewind-files-only")),
                            disabled_reason: file_disabled.clone(),
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
            RewindState::RestoringFiles { .. } | RewindState::ForkingConversation { .. } => None,
        }
    }

    pub(crate) fn activate_rewind_action(
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

    pub(crate) fn start_file_restore(
        &mut self,
        operation_id: u64,
        checkpoint: sessions::ClaudeCheckpoint,
        continue_with_fork: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let outcome = self
            .runtime
            .backend
            .as_mut()
            .map(|session| session.rewind_files(&checkpoint.user_message_id))
            .unwrap_or(SlashCommandOutcome::NotReady);

        match outcome {
            SlashCommandOutcome::Accepted => {}
            SlashCommandOutcome::Rejected { message } => {
                self.palette
                    .set_feedback(CommandFeedbackKind::Error, message, cx);
                return;
            }
            SlashCommandOutcome::NotReady => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Error,
                    translated("agent-rewind-files-not-ready"),
                    cx,
                );
                return;
            }
            SlashCommandOutcome::Completed { message } => {
                self.palette.set_feedback(
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
        self.palette.set_feedback(
            CommandFeedbackKind::Status,
            if continue_with_fork {
                translated("agent-rewind-restoring-before-fork")
            } else {
                translated("agent-rewind-restoring-files")
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
                        this.palette.set_feedback(
                            CommandFeedbackKind::Notice,
                            translated("agent-rewind-files-restored"),
                            cx,
                        );
                    }
                    FileRestoreNext::RetryAction(message) => {
                        this.rewind.state = Some(RewindState::SelectingAction {
                            operation_id,
                            checkpoint,
                        });
                        this.palette.selected = 0;
                        this.palette.set_feedback(
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

    pub(crate) fn start_conversation_fork(
        &mut self,
        operation_id: u64,
        checkpoint: sessions::ClaudeCheckpoint,
        files_restored: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(source_session_id) = self
            .runtime
            .backend
            .as_ref()
            .and_then(Backend::session_id)
            .map(str::to_owned)
        else {
            self.rewind.state = Some(RewindState::SelectingAction {
                operation_id,
                checkpoint,
            });
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                if files_restored {
                    translated("agent-rewind-source-id-missing-after-files")
                } else {
                    translated("agent-rewind-source-id-missing")
                },
                cx,
            );
            return;
        };

        let cwd = self.cwd();
        let user_message_id = checkpoint.user_message_id.clone();
        self.rewind.state = Some(RewindState::ForkingConversation { operation_id });
        self.palette.set_feedback(
            CommandFeedbackKind::Status,
            translated("agent-rewind-creating-prefix"),
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
                        this.palette.set_feedback(
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

    pub(crate) fn replace_with_conversation_fork(
        &mut self,
        fork: sessions::ClaudeFork,
        prompt: String,
        files_restored: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.runtime.backend = None;
        self.clear_conversation_presentation(cx);
        self.palette.skill_catalog = None;
        self.palette.skill_binding = None;
        self.prompts.dismiss_approval();
        reset_command_runtime(
            false,
            &mut self.palette.provider_commands,
            &mut self.palette.provider_commands_ready,
            &mut self.palette.command_queue,
            &mut self.palette.awaiting_command_turn,
            &mut self.palette.selected,
            &mut self.palette.dismissed,
        );
        self.palette.catalog = None;
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
                this.palette.set_feedback(
                    if started {
                        CommandFeedbackKind::Notice
                    } else {
                        CommandFeedbackKind::Error
                    },
                    if !started && files_restored {
                        translated("agent-rewind-start-failed-after-files")
                    } else if !started {
                        translated("agent-rewind-start-failed")
                    } else if files_restored {
                        translated("agent-rewind-complete-with-files")
                    } else {
                        translated("agent-rewind-complete")
                    },
                    cx,
                );
            },
            cx,
        );
    }
}
