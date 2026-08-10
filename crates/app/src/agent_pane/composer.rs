use crate::agent_pane::*;

#[derive(Clone)]
pub(super) struct PendingSlashCommand {
    name: String,
    arguments: String,
}

#[derive(Clone)]
pub(super) enum CommandFeedbackKind {
    Notice,
    Error,
    Queued,
}

#[derive(Clone)]
pub(super) struct CommandFeedback {
    pub(super) kind: CommandFeedbackKind,
    pub(super) message: String,
}

#[derive(Clone)]
pub(super) enum PaletteAction {
    Command(SlashCommandInfo),
    Choice { command: String, value: String },
    Skill(SkillInfo),
    RewindCheckpoint(sessions::ClaudeCheckpoint),
    RewindAction(RewindAction),
}

#[derive(Clone)]
pub(super) struct PaletteRow {
    pub(super) label: String,
    pub(super) description: String,
    pub(super) hint: Option<String>,
    pub(super) disabled_reason: Option<String>,
    pub(super) action: PaletteAction,
}

pub(super) struct PaletteModel {
    pub(super) rows: Vec<PaletteRow>,
    pub(super) note: Option<String>,
}

#[derive(Clone, Copy)]
pub(super) enum PaletteControl {
    Previous,
    Next,
    Activate,
    Complete,
    Dismiss,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RewindAction {
    Files,
    Conversation,
    FilesAndConversation,
    Cancel,
}

pub(super) enum RewindState {
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
    pub(super) fn is_picker(&self) -> bool {
        matches!(
            self,
            Self::Loading { .. } | Self::SelectingCheckpoint { .. } | Self::SelectingAction { .. }
        )
    }

    pub(super) fn has_operation(&self, operation_id: u64) -> bool {
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
pub(super) enum FileRestoreNext {
    Complete,
    ForkConversation,
    RetryAction(String),
}

pub(super) fn file_restore_next(
    continue_with_fork: bool,
    result: Result<(), String>,
) -> FileRestoreNext {
    match (continue_with_fork, result) {
        (true, Ok(())) => FileRestoreNext::ForkConversation,
        (false, Ok(())) => FileRestoreNext::Complete,
        (_, Err(message)) => FileRestoreNext::RetryAction(message),
    }
}

pub(super) fn rewind_blocks_submission(state: Option<&RewindState>) -> bool {
    state.is_some()
}

pub(super) fn rewind_prompt_label(prompt: &str) -> String {
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

pub(super) fn rewind_timestamp(timestamp: Option<&str>) -> Option<String> {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ComposerAction {
    Send,
    Stop,
}

pub(super) fn composer_action(status: Status) -> ComposerAction {
    if status == Status::Running {
        ComposerAction::Stop
    } else {
        ComposerAction::Send
    }
}

pub(super) fn restored_input_after_interruption(submitted: &str, current: &str) -> String {
    if current.trim().is_empty() || current == submitted {
        submitted.to_string()
    } else {
        format!("{submitted}\n\n{current}")
    }
}

impl AgentPane {
    pub(super) fn send_user_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if rewind_blocks_submission(self.rewind.state.as_ref()) {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                "Finish or cancel the current rewind before sending a message.".to_string(),
                cx,
            );
            return;
        }

        let text = self.input.read(cx).text().to_string();

        if parse_slash_command(&text).is_some() {
            self.submit_current_slash(window, cx);
            return;
        }

        let text = text.trim().to_string();

        if text.is_empty() {
            return;
        }

        reconcile_skill_binding(&text, &mut self.palette.skill_binding);
        let skill = if self.kind == AgentKind::Codex {
            match validate_skill_binding(
                &text,
                self.palette.skill_binding.as_ref(),
                self.palette.skill_catalog.as_ref(),
            ) {
                Ok(skill) => skill,
                Err(message) => {
                    self.set_command_feedback(CommandFeedbackKind::Error, message, cx);
                    return;
                }
            }
        } else {
            None
        };

        if self.send_text_with_skill(text, skill.as_ref(), cx) {
            self.palette.skill_binding = None;
            self.input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    pub(super) fn submit_current_slash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.input.read(cx).text().to_string();

        if self.submit_slash_input(&input, cx) {
            self.input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.palette.dismissed = false;
            self.palette.selected = 0;
        }
    }

    /// Route a leading slash before ordinary message handling. Every failure
    /// returns false so the user's input stays available for correction.
    pub(super) fn submit_slash_input(&mut self, input: &str, cx: &mut Context<Self>) -> bool {
        let Some(parsed) = parse_slash_command(input) else {
            return false;
        };
        if parsed.name.is_empty() {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                "Choose a slash command from the list.".to_string(),
                cx,
            );
            return false;
        }

        let Some(command) = self
            .command_catalog()
            .into_iter()
            .find(|command| command.name == parsed.name)
        else {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                format!("Unknown command: /{}", parsed.name),
                cx,
            );
            return false;
        };

        // `/skills` owns a picker stage. A selected row rewrites the
        // composer to `$name`; the slash input itself is never a provider
        // command or an ordinary user turn.
        if command.arguments == SlashCommandArguments::Skills {
            let message = match self.palette.skill_catalog.as_ref() {
                None => "Codex skill discovery is still loading.".to_string(),
                Some(catalog) if catalog.skills.is_empty() && !catalog.errors.is_empty() => {
                    catalog.errors[0].clone()
                }
                Some(catalog) if catalog.skills.is_empty() => {
                    "No Codex skills are available for this folder.".to_string()
                }
                Some(_) => "Choose a skill from the list.".to_string(),
            };

            self.set_command_feedback(CommandFeedbackKind::Error, message, cx);
            return false;
        }

        if command.arguments == SlashCommandArguments::None && !parsed.arguments.trim().is_empty() {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                format!("/{} does not accept arguments.", command.name),
                cx,
            );
            return false;
        }

        if command.arguments == SlashCommandArguments::Choices {
            if parsed.arguments.trim().is_empty() {
                self.set_command_feedback(
                    CommandFeedbackKind::Error,
                    format!("Choose a value for /{}.", command.name),
                    cx,
                );
                return false;
            }

            let choices = self.command_choices(&command.name);
            match resolve_choice(&parsed.arguments, &choices) {
                Ok(value) if command.name == "model" => {
                    self.settings.model = Some(value.clone());
                    self.remember_thread_defaults(cx);
                    self.set_command_feedback(
                        CommandFeedbackKind::Notice,
                        format!("Model set to {value}."),
                        cx,
                    );
                    return true;
                }
                Ok(value) if command.name == "permissions" => {
                    self.settings.approval = Some(value.clone());
                    self.remember_thread_defaults(cx);
                    self.set_command_feedback(
                        CommandFeedbackKind::Notice,
                        format!("Permissions set to {value}."),
                        cx,
                    );
                    return true;
                }
                Ok(_) => {}
                Err(message) => {
                    self.set_command_feedback(CommandFeedbackKind::Error, message, cx);
                    return false;
                }
            }
        }

        match command.name.as_str() {
            "new" | "clear" => {
                if self.is_command_busy() {
                    self.set_command_feedback(
                        CommandFeedbackKind::Error,
                        format!(
                            "/{} is available only while the agent is idle.",
                            command.name
                        ),
                        cx,
                    );
                    false
                } else {
                    self.reset_conversation(cx);
                    true
                }
            }
            "resume" => self.open_recent_sessions(cx),
            "status" => {
                self.show_status(cx);
                true
            }
            "rewind" if self.kind == AgentKind::Claude => self.open_rewind(cx),
            "model" | "permissions" => false,
            _ => self.route_backend_command(
                PendingSlashCommand {
                    name: command.name,
                    arguments: parsed.arguments,
                },
                command.run_policy,
                cx,
            ),
        }
    }

    pub(super) fn route_backend_command(
        &mut self,
        command: PendingSlashCommand,
        policy: SlashCommandRunPolicy,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.is_command_busy() {
            return match policy {
                SlashCommandRunPolicy::QueueUntilIdle => {
                    let name = command.name.clone();
                    self.palette.command_queue.push_back(command);
                    self.set_command_feedback(
                        CommandFeedbackKind::Queued,
                        format!(
                            "Queued /{name} ({} command{} waiting).",
                            self.palette.command_queue.len(),
                            if self.palette.command_queue.len() == 1 {
                                ""
                            } else {
                                "s"
                            }
                        ),
                        cx,
                    );
                    true
                }
                SlashCommandRunPolicy::IdleOnly => {
                    self.set_command_feedback(
                        CommandFeedbackKind::Error,
                        format!(
                            "/{} is available only while the agent is idle.",
                            command.name
                        ),
                        cx,
                    );
                    false
                }
                SlashCommandRunPolicy::Immediate => self.execute_backend_command(command, cx),
            };
        }

        self.execute_backend_command(command, cx)
    }

    pub(super) fn execute_backend_command(
        &mut self,
        command: PendingSlashCommand,
        cx: &mut Context<Self>,
    ) -> bool {
        let outcome = match self.session.as_mut() {
            Some(session) => session.execute_slash_command(&command.name, &command.arguments),
            None => SlashCommandOutcome::NotReady,
        };

        match outcome {
            SlashCommandOutcome::Accepted => {
                self.history_ui.mode = RecentSessionsMode::Hidden;
                self.palette.awaiting_command_turn = true;
                self.set_command_feedback(
                    CommandFeedbackKind::Notice,
                    format!("Starting /{}…", command.name),
                    cx,
                );
                true
            }
            SlashCommandOutcome::Completed { message } => {
                self.set_command_feedback(
                    CommandFeedbackKind::Notice,
                    message.unwrap_or_else(|| format!("/{} completed.", command.name)),
                    cx,
                );
                true
            }
            SlashCommandOutcome::Rejected { message } => {
                self.set_command_feedback(CommandFeedbackKind::Error, message, cx);
                false
            }
            SlashCommandOutcome::NotReady => {
                self.set_command_feedback(
                    CommandFeedbackKind::Error,
                    format!(
                        "{} is still starting; try again in a moment.",
                        self.kind.display()
                    ),
                    cx,
                );
                false
            }
        }
    }

    pub(super) fn run_next_queued_command(&mut self, cx: &mut Context<Self>) {
        if self.is_command_busy() {
            return;
        }
        let Some(command) = self.palette.command_queue.pop_front() else {
            return;
        };

        if !self.execute_backend_command(command, cx) {
            self.palette.command_queue.clear();
        }
    }

    pub(super) fn open_rewind(&mut self, cx: &mut Context<Self>) -> bool {
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

    pub(super) fn cancel_rewind_picker(&mut self, cx: &mut Context<Self>) {
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

    pub(super) fn show_status(&mut self, cx: &mut Context<Self>) {
        let status = match self.status {
            Status::Starting => "starting",
            Status::Idle => "idle",
            Status::Running => "running",
            Status::Exited => "exited",
        };
        let mut fields = vec![
            format!("backend={}", self.kind.display()),
            format!("status={status}"),
        ];

        for (name, value) in [
            ("model", self.settings.model.as_deref()),
            ("permissions", self.settings.approval.as_deref()),
            ("sandbox", self.settings.sandbox.as_deref()),
            ("effort", self.settings.effort.as_deref()),
            ("tier", self.settings.tier.as_deref()),
        ] {
            if let Some(value) = value {
                fields.push(format!("{name}={value}"));
            }
        }
        if !self.palette.command_queue.is_empty() {
            fields.push(format!("queued={}", self.palette.command_queue.len()));
        }

        self.set_command_feedback(CommandFeedbackKind::Notice, fields.join(" · "), cx);
    }

    pub(super) fn open_recent_sessions(&mut self, cx: &mut Context<Self>) -> bool {
        if self.is_command_busy() {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                "/resume is available only while the agent is idle.".to_string(),
                cx,
            );
            return false;
        }

        let rows = self
            .history_ui
            .pending
            .unwrap_or(self.history_ui.sessions.len());
        if rows == 0 {
            self.history_ui.mode = RecentSessionsMode::Hidden;
            self.set_command_feedback(
                CommandFeedbackKind::Notice,
                "No recent sessions are available for this folder.".to_string(),
                cx,
            );
            return true;
        }

        self.history_ui.mode = RecentSessionsMode::Open;
        self.history_ui.selected = 0;
        self.palette.feedback = None;
        cx.notify();
        true
    }

    pub(super) fn set_command_feedback(
        &mut self,
        kind: CommandFeedbackKind,
        message: String,
        cx: &mut Context<Self>,
    ) {
        self.palette.feedback = Some(CommandFeedback { kind, message });
        cx.notify();
    }

    pub(super) fn is_command_busy(&self) -> bool {
        self.status == Status::Running
            || self.palette.awaiting_command_turn
            || self.history_ui.mode == RecentSessionsMode::Loading
            || rewind_blocks_submission(self.rewind.state.as_ref())
    }

    pub(super) fn skill_disabled_reason(&self, skill: &SkillInfo) -> Option<String> {
        if !skill.enabled {
            Some("Disabled by Codex".to_string())
        } else if matches!(self.status, Status::Starting | Status::Exited) {
            Some(match self.status {
                Status::Starting => "Agent is still starting".to_string(),
                Status::Exited => "Agent has exited".to_string(),
                _ => unreachable!(),
            })
        } else {
            None
        }
    }

    pub(super) fn command_catalog(&self) -> Vec<SlashCommandInfo> {
        let adapter = self
            .session
            .as_ref()
            .map(Backend::adapter_commands)
            .unwrap_or_else(|| match self.kind {
                AgentKind::Codex => app_server::Session::adapter_commands(),
                AgentKind::Claude => stream_json::Session::adapter_commands(),
            });

        merge_catalog(
            local_commands(),
            adapter,
            self.palette.provider_commands.clone(),
        )
    }

    pub(super) fn command_choices(&self, command: &str) -> Vec<(String, String)> {
        match command {
            "model" => self
                .models
                .iter()
                .map(|model| (model.model.clone(), model.display.clone()))
                .collect(),
            "permissions" => match self.kind {
                AgentKind::Codex => app_server::APPROVAL_OPTIONS
                    .iter()
                    .map(|value| (value.to_string(), value.to_string()))
                    .collect(),
                AgentKind::Claude => stream_json::PERMISSION_OPTIONS
                    .iter()
                    .map(|value| (value.to_string(), value.to_string()))
                    .collect(),
            },
            _ => Vec::new(),
        }
    }

    pub(super) fn palette_model(&self, cx: &Context<Self>) -> Option<PaletteModel> {
        if let Some(state) = self.rewind.state.as_ref() {
            return self.rewind_palette_model(state);
        }
        if self.palette.dismissed {
            return None;
        }

        let input = self.input.read(cx);
        let text = input.text().to_string();
        let parsed = parse_slash_command(&text)?;
        let cursor = input.cursor();
        let catalog = self.command_catalog();

        if parsed.has_argument_separator {
            let command = catalog.iter().find(|command| command.name == parsed.name)?;

            if command.arguments == SlashCommandArguments::Skills {
                let query = parsed.arguments.trim().to_ascii_lowercase();
                let Some(skill_catalog) = self.palette.skill_catalog.as_ref() else {
                    return Some(PaletteModel {
                        rows: Vec::new(),
                        note: Some("Codex skill discovery is still loading".to_string()),
                    });
                };
                let rows = filter_skill_catalog(&skill_catalog.skills, &query)
                    .into_iter()
                    .map(|skill| {
                        let disabled_reason = self.skill_disabled_reason(&skill);

                        PaletteRow {
                            label: format!("${}", skill.name),
                            description: skill.description.clone(),
                            hint: Some(skill.scope.clone()),
                            disabled_reason,
                            action: PaletteAction::Skill(skill),
                        }
                    })
                    .collect::<Vec<_>>();
                let note = if rows.is_empty() && !skill_catalog.errors.is_empty() {
                    Some(skill_catalog.errors[0].clone())
                } else if rows.is_empty() && query.is_empty() {
                    Some("No Codex skills are available for this folder".to_string())
                } else if rows.is_empty() {
                    Some("No matching skills".to_string())
                } else if let Some(error) = skill_catalog.errors.first() {
                    Some(format!("Some skills could not be loaded: {error}"))
                } else {
                    None
                };

                return Some(PaletteModel { rows, note });
            }

            if command.arguments != SlashCommandArguments::Choices {
                return None;
            }

            let query = parsed.arguments.to_ascii_lowercase();
            let rows = self
                .command_choices(&command.name)
                .into_iter()
                .filter(|(value, label)| {
                    query.is_empty()
                        || value.to_ascii_lowercase().contains(&query)
                        || label.to_ascii_lowercase().contains(&query)
                })
                .map(|(value, label)| PaletteRow {
                    description: value.clone(),
                    label,
                    hint: None,
                    disabled_reason: None,
                    action: PaletteAction::Choice {
                        command: command.name.clone(),
                        value,
                    },
                })
                .collect::<Vec<_>>();

            return Some(PaletteModel {
                note: rows.is_empty().then(|| "No matching values".to_string()),
                rows,
            });
        }

        // Moving the caret into later prose must not turn an ordinary edit
        // into palette navigation; only the first slash token owns the keys.
        if cursor > 1 + parsed.name.len() {
            return None;
        }

        let skills: &[SkillInfo] = if self.kind == AgentKind::Codex {
            self.palette
                .skill_catalog
                .as_ref()
                .map(|catalog| catalog.skills.as_slice())
                .unwrap_or_default()
        } else {
            &[]
        };
        let rows = filter_palette_catalog(&catalog, skills, &parsed.name)
            .into_iter()
            .map(|entry| match entry {
                PaletteCatalogEntry::Command(command) => {
                    let disabled_reason = if command.run_policy == SlashCommandRunPolicy::IdleOnly
                        && self.is_command_busy()
                    {
                        Some("Available when the agent is idle".to_string())
                    } else if command.source != SlashCommandSource::Local
                        && matches!(self.status, Status::Starting | Status::Exited)
                    {
                        Some(match self.status {
                            Status::Starting => "Agent is still starting".to_string(),
                            Status::Exited => "Agent has exited".to_string(),
                            _ => unreachable!(),
                        })
                    } else {
                        None
                    };

                    PaletteRow {
                        label: format!("/{}", command.name),
                        description: command.description.clone(),
                        hint: command.argument_hint.clone(),
                        disabled_reason,
                        action: PaletteAction::Command(command),
                    }
                }
                PaletteCatalogEntry::Skill(skill) => PaletteRow {
                    label: format!("/{}", skill.name),
                    description: skill.description.clone(),
                    hint: Some(format!("skill · {}", skill.scope)),
                    disabled_reason: self.skill_disabled_reason(&skill),
                    action: PaletteAction::Skill(skill),
                },
            })
            .collect::<Vec<_>>();
        let note = if rows.is_empty() {
            if self.kind == AgentKind::Codex && self.palette.skill_catalog.is_none() {
                Some("Codex skill discovery is still loading".to_string())
            } else if self.kind == AgentKind::Codex
                && self
                    .palette
                    .skill_catalog
                    .as_ref()
                    .is_some_and(|catalog| !catalog.errors.is_empty())
            {
                self.palette
                    .skill_catalog
                    .as_ref()
                    .and_then(|catalog| catalog.errors.first().cloned())
            } else if self.kind == AgentKind::Codex {
                Some("No matching commands or skills".to_string())
            } else {
                Some("No matching commands".to_string())
            }
        } else if self.kind == AgentKind::Claude && !self.palette.provider_commands_ready {
            Some("Claude command discovery is still loading".to_string())
        } else if self.kind == AgentKind::Codex && self.palette.skill_catalog.is_none() {
            Some("Codex skill discovery is still loading".to_string())
        } else if self.kind == AgentKind::Codex {
            self.palette
                .skill_catalog
                .as_ref()
                .and_then(|catalog| catalog.errors.first())
                .map(|error| format!("Some skills could not be loaded: {error}"))
        } else {
            None
        };

        Some(PaletteModel { rows, note })
    }

    pub(super) fn rewind_palette_model(&self, state: &RewindState) -> Option<PaletteModel> {
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

    pub(super) fn handle_palette_control(
        &mut self,
        control: PaletteControl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.palette_model(cx) else {
            if self.handle_recent_sessions_control(control, cx) {
                return;
            }
            cx.propagate();
            return;
        };

        cx.stop_propagation();

        match control {
            PaletteControl::Previous | PaletteControl::Next => {
                let direction = match control {
                    PaletteControl::Previous => PaletteDirection::Previous,
                    PaletteControl::Next => PaletteDirection::Next,
                    _ => unreachable!(),
                };

                if let Some(selected) =
                    move_palette_selection(self.palette.selected, model.rows.len(), direction)
                {
                    self.palette.selected = selected;
                    self.palette.scroll.scroll_to_item(self.palette.selected);
                    cx.notify();
                }
            }
            PaletteControl::Activate => {
                if model.rows.is_empty() {
                    self.submit_current_slash(window, cx);
                } else {
                    self.activate_palette_index(self.palette.selected, true, window, cx);
                }
            }
            PaletteControl::Complete => {
                self.activate_palette_index(self.palette.selected, false, window, cx);
            }
            PaletteControl::Dismiss => {
                self.dismiss_command_palette(cx);
            }
        }
    }

    fn dismiss_command_palette(&mut self, cx: &mut Context<Self>) {
        if self
            .rewind
            .state
            .as_ref()
            .is_some_and(RewindState::is_picker)
        {
            self.cancel_rewind_picker(cx);
        } else {
            self.palette.dismissed = true;
            cx.notify();
        }
    }

    fn handle_recent_sessions_control(
        &mut self,
        control: PaletteControl,
        cx: &mut Context<Self>,
    ) -> bool {
        if matches!(control, PaletteControl::Complete) || self.input.read(cx).text().len() != 0 {
            return false;
        }

        let rows = self
            .history_ui
            .pending
            .unwrap_or(self.history_ui.sessions.len());
        if !self.history_ui.mode.is_visible(self.items.is_empty(), rows) {
            return false;
        }

        cx.stop_propagation();

        match control {
            PaletteControl::Previous | PaletteControl::Next => {
                let direction = match control {
                    PaletteControl::Previous => PaletteDirection::Previous,
                    PaletteControl::Next => PaletteDirection::Next,
                    _ => unreachable!(),
                };

                if let Some(selected) = move_palette_selection(
                    self.history_ui.selected,
                    self.history_ui.sessions.len(),
                    direction,
                ) {
                    self.history_ui.selected = selected;
                    self.history_ui
                        .scroll
                        .scroll_to_item(selected, ScrollStrategy::Nearest);
                    cx.notify();
                }
            }
            PaletteControl::Activate => {
                self.resume_session(self.history_ui.selected, cx);
            }
            PaletteControl::Dismiss => {
                self.history_ui.mode = RecentSessionsMode::Hidden;
                cx.notify();
            }
            PaletteControl::Complete => unreachable!(),
        }

        true
    }

    pub(super) fn activate_palette_index(
        &mut self,
        index: usize,
        execute: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self
            .palette_model(cx)
            .and_then(|model| model.rows.get(index).cloned())
        else {
            return;
        };

        if let Some(reason) = row.disabled_reason {
            self.set_command_feedback(CommandFeedbackKind::Error, reason, cx);
            return;
        }

        let (text, can_execute) = match row.action {
            PaletteAction::Command(command) => {
                let needs_arguments = command.arguments != SlashCommandArguments::None;
                (
                    format!(
                        "/{}{}",
                        command.name,
                        if needs_arguments { " " } else { "" }
                    ),
                    !needs_arguments,
                )
            }
            PaletteAction::Choice { command, value } => (format!("/{command} {value}"), true),
            PaletteAction::Skill(skill) => {
                let Ok((text, binding)) = prepare_skill_selection(&skill) else {
                    self.set_command_feedback(
                        CommandFeedbackKind::Error,
                        format!("${} is disabled by Codex.", skill.name),
                        cx,
                    );
                    return;
                };

                self.input.update(cx, |input, cx| {
                    input.set_value(text.clone(), window, cx);
                    input.set_selected_range(text.len()..text.len(), cx);
                });
                self.palette.skill_binding = Some(binding);
                self.palette.selected = 0;
                self.palette.dismissed = true;
                cx.notify();
                return;
            }
            PaletteAction::RewindCheckpoint(checkpoint) => {
                let Some(operation_id) = self.rewind.state.as_ref().and_then(|state| match state {
                    RewindState::SelectingCheckpoint { operation_id, .. } => Some(*operation_id),
                    _ => None,
                }) else {
                    return;
                };
                self.rewind.state = Some(RewindState::SelectingAction {
                    operation_id,
                    checkpoint,
                });
                self.palette.selected = 0;
                cx.notify();
                return;
            }
            PaletteAction::RewindAction(action) => {
                self.activate_rewind_action(action, window, cx);
                return;
            }
        };

        self.input.update(cx, |input, cx| {
            input.set_value(text.clone(), window, cx);
            input.set_selected_range(text.len()..text.len(), cx);
        });
        self.palette.selected = 0;

        if execute && can_execute {
            self.submit_current_slash(window, cx);
        } else {
            cx.notify();
        }
    }

    pub(super) fn activate_rewind_action(
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

    pub(super) fn start_file_restore(
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

    pub(super) fn start_conversation_fork(
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

    pub(super) fn replace_with_conversation_fork(
        &mut self,
        fork: sessions::ClaudeFork,
        prompt: String,
        files_restored: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.session = None;
        self.clear_conversation_presentation();
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

    pub(super) fn render_command_palette(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let model = self.palette_model(cx)?;
        let selected = self
            .palette
            .selected
            .min(model.rows.len().saturating_sub(1));
        let rows = model
            .rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let disabled = row.disabled_reason.is_some();
                let detail = row.disabled_reason.clone().unwrap_or(row.description);
                let background = (index == selected).then(|| cx.theme().muted.opacity(0.7));

                div()
                    .id(("agent-slash-command", index))
                    .h(px(48.))
                    .flex_none()
                    .px_3()
                    .py_1p5()
                    .rounded(UI_RADIUS)
                    .when_some(background, |this, color| this.bg(color))
                    .when(disabled, |this| this.opacity(0.5))
                    .when(!disabled, |this| {
                        this.hover(|style| style.bg(cx.theme().muted.opacity(0.45)))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_palette_index(index, true, window, cx)
                    }))
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().foreground)
                                    .child(row.label),
                            )
                            .children(row.hint.map(|hint| {
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground.opacity(0.75))
                                    .child(hint)
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .text_color(cx.theme().muted_foreground)
                            .child(detail),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let note = model.note.map(|note| {
            div()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground.opacity(0.75))
                .child(note)
        });

        Some(
            v_flex()
                .id("agent-slash-command-palette")
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss_command_palette(cx)))
                .w_full()
                .max_h(px(9. * 48. + 36.))
                .overflow_y_scroll()
                .track_scroll(&self.palette.scroll)
                .p_1()
                .rounded(UI_RADIUS)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().popover)
                .shadow_lg()
                .children(rows)
                .children(note)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod rewind_state_tests {
    use nmt_agent_utils::chat::SlashCommandRunPolicy;

    use super::{
        FileRestoreNext, RewindState, app_server, file_restore_next,
        restored_input_after_interruption, rewind_blocks_submission, sessions, stream_json,
    };

    fn checkpoint() -> sessions::ClaudeCheckpoint {
        sessions::ClaudeCheckpoint {
            user_message_id: "00000000-0000-4000-8000-000000000001".into(),
            parent_message_id: None,
            prompt: "recover this prompt".into(),
            timestamp: Some("2026-08-07T01:00:00Z".into()),
            file_restore_availability: sessions::FileRestoreAvailability::Available,
        }
    }

    #[test]
    fn picker_cancellation_and_processing_phases_are_distinct() {
        let picker_states = [
            RewindState::Loading { operation_id: 7 },
            RewindState::SelectingCheckpoint {
                operation_id: 7,
                checkpoints: vec![checkpoint()],
            },
            RewindState::SelectingAction {
                operation_id: 7,
                checkpoint: checkpoint(),
            },
        ];
        for state in &picker_states {
            assert!(state.is_picker());
            assert!(state.has_operation(7));
            assert!(!state.has_operation(6), "stale operations must be ignored");
            assert!(rewind_blocks_submission(Some(state)));
        }

        for state in [
            RewindState::RestoringFiles { operation_id: 7 },
            RewindState::ForkingConversation { operation_id: 7 },
        ] {
            assert!(!state.is_picker());
            assert!(rewind_blocks_submission(Some(&state)));
        }
        assert!(!rewind_blocks_submission(None));
    }

    #[test]
    fn file_phase_success_and_failure_choose_the_safe_next_step() {
        assert_eq!(file_restore_next(false, Ok(())), FileRestoreNext::Complete);
        assert_eq!(
            file_restore_next(true, Ok(())),
            FileRestoreNext::ForkConversation
        );
        assert_eq!(
            file_restore_next(true, Err("expired checkpoint".into())),
            FileRestoreNext::RetryAction("expired checkpoint".into())
        );
    }

    #[test]
    fn rewind_catalog_is_claude_only_and_idle_only() {
        let claude = stream_json::Session::adapter_commands();
        let rewind = claude
            .iter()
            .find(|command| command.name == "rewind")
            .expect("Claude rewind command");

        assert_eq!(rewind.run_policy, SlashCommandRunPolicy::IdleOnly);
        assert!(
            app_server::Session::adapter_commands()
                .iter()
                .all(|command| command.name != "rewind")
        );
    }

    #[test]
    fn interrupted_prompt_returns_without_discarding_a_new_draft() {
        assert_eq!(
            restored_input_after_interruption("original prompt", ""),
            "original prompt"
        );
        assert_eq!(
            restored_input_after_interruption("original prompt", "new draft"),
            "original prompt\n\nnew draft"
        );
        assert_eq!(
            restored_input_after_interruption("original prompt", "original prompt"),
            "original prompt"
        );
    }
}
