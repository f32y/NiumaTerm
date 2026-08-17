mod palette;
mod rewind;

#[allow(unused_imports)]
pub(super) use crate::agent_pane::composer::palette::{
    PaletteAction, PaletteControl, PaletteModel, PaletteRow,
};
#[allow(unused_imports)]
pub(super) use crate::agent_pane::composer::rewind::{
    FileRestoreNext, RewindAction, RewindState, file_restore_next, rewind_blocks_submission,
    rewind_prompt_label, rewind_timestamp,
};
#[cfg(test)]
mod tests;

use nmt_i18n::i18n;

use crate::agent_pane::*;

#[derive(Clone)]
pub(super) struct PendingSlashCommand {
    name: String,
    arguments: String,
}

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

/// How long an acknowledgement stays before retiring itself. Long enough to
/// read after a glance away, short enough that it does not outlive the command
/// it describes.
const FEEDBACK_LIFETIME: Duration = Duration::from_secs(6);

/// Whether a message still describes the situation. A queued message counts
/// the command queue, and several paths empty that queue without going through
/// the palette -- a failed spawn, an update stopping active work, a
/// conversation reset. Deciding this where the message is shown keeps a future
/// path from reintroducing a count of commands that are no longer waiting.
fn feedback_is_current(kind: CommandFeedbackKind, queue_is_empty: bool) -> bool {
    !(kind == CommandFeedbackKind::Queued && queue_is_empty)
}

/// Whether a message is a passing acknowledgement rather than something the
/// user still has to act on. An error stays until it is read, and a queued
/// list describes work still waiting rather than work already accepted.
fn feedback_is_transient(kind: CommandFeedbackKind) -> bool {
    match kind {
        CommandFeedbackKind::Notice => true,
        CommandFeedbackKind::Status | CommandFeedbackKind::Error | CommandFeedbackKind::Queued => {
            false
        }
    }
}

#[derive(Clone)]
pub(super) struct CommandFeedback {
    pub(super) kind: CommandFeedbackKind,
    pub(super) message: String,
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
                i18n("agent-session-rewind-blocks-send").to_string(),
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
        let skill = if self.kind.caps().skill_references {
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

        if self.send_text_with_skill(text.clone(), skill.as_ref(), cx) {
            self.record_input_history(&text, cx);
            self.palette.skill_binding = None;
            self.input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    pub(super) fn submit_current_slash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.input.read(cx).text().to_string();

        if self.submit_slash_input(&input, cx) {
            self.record_input_history(&input, cx);
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
                i18n("agent-composer-choose-command").to_string(),
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
                i18n("agent-composer-unknown-command").replace("{name}", &parsed.name),
                cx,
            );
            return false;
        };

        // `/skills` owns a picker stage. A selected row rewrites the
        // composer to `$name`; the slash input itself is never a provider
        // command or an ordinary user turn.
        if command.arguments == SlashCommandArguments::Skills {
            let message = match self.palette.skill_catalog.as_ref() {
                None => i18n("agent-composer-skill-discovery-loading-period").to_string(),
                Some(catalog) if catalog.skills.is_empty() && !catalog.errors.is_empty() => {
                    catalog.errors[0].clone()
                }
                Some(catalog) if catalog.skills.is_empty() => {
                    i18n("agent-composer-no-skills-period").to_string()
                }
                Some(_) => i18n("agent-composer-choose-skill").to_string(),
            };

            self.set_command_feedback(CommandFeedbackKind::Error, message, cx);
            return false;
        }

        if command.arguments == SlashCommandArguments::None && !parsed.arguments.trim().is_empty() {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-composer-command-no-arguments").replace("{name}", &command.name),
                cx,
            );
            return false;
        }

        if command.arguments == SlashCommandArguments::Choices {
            if parsed.arguments.trim().is_empty() {
                self.set_command_feedback(
                    CommandFeedbackKind::Error,
                    i18n("agent-composer-choose-value").replace("{name}", &command.name),
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
                        i18n("agent-composer-model-set").replace("{value}", &value),
                        cx,
                    );
                    // Where the harness adopts a model through its own request,
                    // recording the pick is not applying it. This runs after the
                    // notice so a refusal replaces it rather than hiding under
                    // a confirmation of something that did not happen.
                    if self.kind.caps().model_selection_is_a_request {
                        self.apply_model_selection(cx);
                    }
                    return true;
                }
                Ok(value) if command.name == "permissions" => {
                    self.settings.approval = Some(value.clone());
                    self.remember_thread_defaults(cx);
                    self.set_command_feedback(
                        CommandFeedbackKind::Notice,
                        i18n("agent-composer-permissions-set")
                            .replace("{value}", &setting_value_label(&value)),
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
                        i18n("agent-composer-command-idle-only").replace("{name}", &command.name),
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
            "rewind" if self.kind.caps().file_rewind => self.open_rewind(cx),
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
                    let count = self.palette.command_queue.len();
                    self.set_command_feedback(
                        CommandFeedbackKind::Queued,
                        i18n(if count == 1 {
                            "agent-composer-command-queued-one"
                        } else {
                            "agent-composer-command-queued-many"
                        })
                        .replace("{name}", &name)
                        .replace("{count}", &count.to_string()),
                        cx,
                    );
                    true
                }
                SlashCommandRunPolicy::IdleOnly => {
                    self.set_command_feedback(
                        CommandFeedbackKind::Error,
                        i18n("agent-composer-command-idle-only").replace("{name}", &command.name),
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
                    i18n("agent-composer-command-starting").replace("{name}", &command.name),
                    cx,
                );
                true
            }
            SlashCommandOutcome::Completed { message } => {
                self.set_command_feedback(
                    CommandFeedbackKind::Notice,
                    message.unwrap_or_else(|| {
                        i18n("agent-session-command-completed").replace("{name}", &command.name)
                    }),
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
                    i18n("agent-session-still-starting").replace("{name}", self.kind.display()),
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

    pub(super) fn show_status(&mut self, cx: &mut Context<Self>) {
        let status = match self.status {
            Status::Starting => i18n("agent-composer-status-starting"),
            Status::Idle => i18n("agent-composer-status-idle"),
            Status::Running => i18n("agent-composer-status-running"),
            Status::Exited => i18n("agent-composer-status-exited"),
        };
        let mut fields = vec![
            i18n("agent-composer-status-field")
                .replace("{name}", i18n("agent-composer-status-backend"))
                .replace("{value}", self.kind.display()),
            i18n("agent-composer-status-field")
                .replace("{name}", i18n("agent-composer-status-label"))
                .replace("{value}", status),
        ];

        for (name, value) in [
            (i18n("agent-setting-model"), self.settings.model.as_deref()),
            (
                i18n("agent-setting-permissions"),
                self.settings.approval.as_deref(),
            ),
            (
                i18n("agent-setting-sandbox"),
                self.settings.sandbox.as_deref(),
            ),
            (
                i18n("agent-setting-effort"),
                self.settings.effort.as_deref(),
            ),
            (i18n("agent-setting-tier"), self.settings.tier.as_deref()),
        ] {
            if let Some(value) = value {
                fields.push(
                    i18n("agent-composer-status-field")
                        .replace("{name}", name)
                        .replace("{value}", value),
                );
            }
        }
        if !self.palette.command_queue.is_empty() {
            fields.push(
                i18n("agent-composer-status-field")
                    .replace("{name}", i18n("agent-composer-status-queued"))
                    .replace("{value}", &self.palette.command_queue.len().to_string()),
            );
        }

        // Answering /status is information the user asked for, so it holds
        // rather than fading out from under them.
        self.set_command_feedback(CommandFeedbackKind::Status, fields.join(" · "), cx);
    }

    pub(super) fn set_command_feedback(
        &mut self,
        kind: CommandFeedbackKind,
        message: String,
        cx: &mut Context<Self>,
    ) {
        self.palette.feedback_seq += 1;
        let seq = self.palette.feedback_seq;
        self.palette.feedback = Some(CommandFeedback { kind, message });
        cx.notify();

        if !feedback_is_transient(kind) {
            return;
        }

        // A notice acknowledges a request before anything visible happens. A
        // command that then runs a whole turn fills the transcript with its
        // real answer, and the acknowledgement above the composer becomes a
        // line the user cannot dismiss, because only typing clears it.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FEEDBACK_LIFETIME).await;
            let _ = this.update(cx, |this, cx| {
                if this.palette.feedback_seq == seq {
                    this.palette.feedback = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The message worth showing right now, if any.
    pub(in crate::agent_pane) fn visible_command_feedback(&self) -> Option<&CommandFeedback> {
        self.palette.feedback.as_ref().filter(|feedback| {
            feedback_is_current(feedback.kind, self.palette.command_queue.is_empty())
        })
    }

    pub(super) fn is_command_busy(&self) -> bool {
        self.status == Status::Running
            || self.palette.awaiting_command_turn
            || self.history_ui.mode == RecentSessionsMode::Loading
            || rewind_blocks_submission(self.rewind.state.as_ref())
    }

    pub(super) fn skill_disabled_reason(&self, skill: &SkillInfo) -> Option<String> {
        if !skill.enabled {
            Some(i18n("agent-composer-disabled-by-codex").to_string())
        } else if matches!(self.status, Status::Starting | Status::Exited) {
            Some(match self.status {
                Status::Starting => i18n("agent-composer-agent-starting").to_string(),
                Status::Exited => i18n("agent-composer-agent-exited").to_string(),
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
                // DeepSeek's commands live behind its own registry and are not
                // mapped yet, so the palette offers only the local ones.
                AgentKind::DeepSeek => Vec::new(),
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
                    .map(|value| (value.to_string(), setting_value_label(value)))
                    .collect(),
                AgentKind::Claude => stream_json::PERMISSION_OPTIONS
                    .iter()
                    .map(|value| (value.to_string(), setting_value_label(value)))
                    .collect(),
                // Changing the DeepSeek sandbox preset mid-session is part of
                // the approval work, so the command offers no choices yet.
                AgentKind::DeepSeek => Vec::new(),
            },
            _ => Vec::new(),
        }
    }
}
