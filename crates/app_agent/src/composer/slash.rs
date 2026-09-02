//! A slash line from the moment it is submitted to the moment it runs.
//!
//! A `/` line steers the session rather than continuing the conversation, and
//! which side owns a given command differs: some the pane answers itself, the
//! rest go to the harness and come back as a result. Routing is what decides
//! between them, and the queue is what keeps a command from overtaking a turn.

use std::rc::Rc;

use gpui::{Context, SharedString, Window};
use nmt_agent_utils::chat::{
    SkillInfo, SlashCommandArguments, SlashCommandInfo, SlashCommandOutcome, SlashCommandRunPolicy,
};
use nmt_agent_utils::claude_code::stream_json;
use nmt_agent_utils::codex::app_server;
use nmt_agent_utils::deepseek;
use nmt_i18n::i18n;

use crate::commands::{
    local_commands, merge_catalog, parse_slash_command, resolve_choice, setting_value_label,
};
use crate::composer::{CommandFeedbackKind, PendingSlashCommand};
use crate::profile::AgentKind;
use crate::session::{Backend, Status};
use crate::{AgentPane, CachedCatalog, RecentSessionsMode, translated};

impl AgentPane {
    pub(crate) fn submit_current_slash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.input.read(cx).text().to_string();

        if self.submit_slash_input(&input, cx) {
            self.record_input_history(&input, cx);
            self.input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.palette.dismissed = false;
            self.palette.selected = 0;
        }
    }

    /// Whether the discovered skill catalog carries this name.
    fn names_a_skill(&self, name: &str) -> bool {
        self.palette
            .skill_catalog
            .as_ref()
            .is_some_and(|catalog| catalog.skills.iter().any(|skill| skill.name == name))
    }

    /// Route a leading slash before ordinary message handling. Every failure
    /// returns false so the user's input stays available for correction.
    pub(super) fn submit_slash_input(&mut self, input: &str, cx: &mut Context<Self>) -> bool {
        let Some(parsed) = parse_slash_command(input) else {
            return false;
        };
        if parsed.name.is_empty() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-composer-choose-command").to_string(),
                cx,
            );
            return false;
        }

        let catalog = self.command_catalog();
        let matched = catalog
            .iter()
            .find(|command| command.name == parsed.name)
            .cloned();

        let Some(command) = matched else {
            // Where a skill is invoked by writing its name into the prompt, a
            // slash line naming one is a message the harness expands, so
            // refusing it as an unknown command would block the only way to
            // reach a skill at all.
            if self.kind.caps().slash_skills_are_prompts && self.names_a_skill(&parsed.name) {
                return self.send_text(input.to_string(), cx);
            }

            self.palette.set_feedback(
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

            self.palette
                .set_feedback(CommandFeedbackKind::Error, message, cx);
            return false;
        }

        if command.arguments == SlashCommandArguments::None && !parsed.arguments.trim().is_empty() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-composer-command-no-arguments").replace("{name}", &command.name),
                cx,
            );
            return false;
        }

        if command.arguments == SlashCommandArguments::Choices {
            if parsed.arguments.trim().is_empty() {
                self.palette.set_feedback(
                    CommandFeedbackKind::Error,
                    i18n("agent-composer-choose-value").replace("{name}", &command.name),
                    cx,
                );
                return false;
            }

            let choices = self.command_choices(&command.name);
            match resolve_choice(&parsed.arguments, &choices) {
                Ok(value) if command.name == "model" => {
                    self.controls.settings.model = Some(value.clone());
                    self.remember_thread_defaults(cx);
                    self.palette.set_feedback(
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
                    self.controls.settings.approval = Some(value.clone());
                    self.remember_thread_defaults(cx);
                    self.palette.set_feedback(
                        CommandFeedbackKind::Notice,
                        i18n("agent-composer-permissions-set")
                            .replace("{value}", &setting_value_label(&value)),
                        cx,
                    );
                    return true;
                }
                Ok(_) => {}
                Err(message) => {
                    self.palette
                        .set_feedback(CommandFeedbackKind::Error, message, cx);
                    return false;
                }
            }
        }

        match command.name.as_str() {
            "new" | "clear" => {
                if self.is_command_busy() {
                    self.palette.set_feedback(
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
            "rename" if self.kind.caps().session_rename => {
                self.rename_conversation(&parsed.arguments, cx)
            }
            "fork" if self.kind.caps().session_fork => self.open_fork(cx),
            // Where the conversation is a file this side rewrites, the rewind
            // picker cuts the same branch and offers restoring the files that
            // turn touched alongside it. Opening a second picker for the
            // smaller half of what one command already does would only hide
            // the choice behind the name it was reached by.
            "fork" if self.kind.caps().file_rewind => self.open_rewind(cx),
            "find" if self.kind.caps().session_search => {
                self.search_conversations(&parsed.arguments, cx)
            }
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
                    self.palette.set_feedback(
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
                    self.palette.set_feedback(
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

    pub(crate) fn execute_backend_command(
        &mut self,
        command: PendingSlashCommand,
        cx: &mut Context<Self>,
    ) -> bool {
        let outcome = match self.runtime.backend.as_mut() {
            Some(session) => session.execute_slash_command(&command.name, &command.arguments),
            None => SlashCommandOutcome::NotReady,
        };

        match outcome {
            SlashCommandOutcome::Accepted => {
                self.history_ui.mode = RecentSessionsMode::Hidden;
                self.palette.awaiting_command_turn = true;
                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    i18n("agent-composer-command-starting").replace("{name}", &command.name),
                    cx,
                );
                true
            }
            SlashCommandOutcome::Completed { message } => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    message.unwrap_or_else(|| {
                        i18n("agent-session-command-completed").replace("{name}", &command.name)
                    }),
                    cx,
                );
                true
            }
            SlashCommandOutcome::Rejected { message } => {
                self.palette
                    .set_feedback(CommandFeedbackKind::Error, message, cx);
                false
            }
            SlashCommandOutcome::NotReady => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Error,
                    i18n("agent-session-still-starting").replace("{name}", self.kind.display()),
                    cx,
                );
                false
            }
        }
    }

    pub(crate) fn run_next_queued_command(&mut self, cx: &mut Context<Self>) {
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
        let status = match self.runtime.status {
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
            (
                i18n("agent-setting-model"),
                self.controls.settings.model.as_deref(),
            ),
            (
                i18n("agent-setting-permissions"),
                self.controls.settings.approval.as_deref(),
            ),
            (
                i18n("agent-setting-sandbox"),
                self.controls.settings.sandbox.as_deref(),
            ),
            (
                i18n("agent-setting-effort"),
                self.controls.settings.effort.as_deref(),
            ),
            (
                i18n("agent-setting-tier"),
                self.controls.settings.tier.as_deref(),
            ),
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
        self.palette
            .set_feedback(CommandFeedbackKind::Status, fields.join(" · "), cx);
    }

    pub(super) fn skill_disabled_reason(&self, skill: &SkillInfo) -> Option<SharedString> {
        if !skill.enabled {
            Some(translated("agent-composer-disabled-by-codex"))
        } else {
            // A skill is invoked through the harness, so it needs a session
            // that has finished starting and has not ended.
            match self.runtime.status {
                Status::Starting => Some(translated("agent-composer-agent-starting")),
                Status::Exited => Some(translated("agent-composer-agent-exited")),
                _ => None,
            }
        }
    }

    pub(crate) fn command_catalog(&mut self) -> Rc<[SlashCommandInfo]> {
        let language = nmt_i18n::active_language();

        if let Some(cached) = self
            .palette
            .catalog
            .as_ref()
            .filter(|cached| cached.language == language)
        {
            return cached.commands.clone();
        }

        let adapter = self
            .runtime
            .backend
            .as_ref()
            .map(Backend::adapter_commands)
            .unwrap_or_else(|| match self.kind {
                AgentKind::Codex => app_server::Session::adapter_commands(),
                AgentKind::Claude => stream_json::Session::adapter_commands(),
                AgentKind::DeepSeek => deepseek::Session::adapter_commands(),
            });

        let commands: Rc<[SlashCommandInfo]> = merge_catalog(
            local_commands(),
            adapter,
            self.palette.provider_commands.clone(),
        )
        .into();

        self.palette.catalog = Some(CachedCatalog {
            language,
            commands: commands.clone(),
        });

        commands
    }

    pub(super) fn command_choices(&self, command: &str) -> Vec<(String, String)> {
        match command {
            "model" => self
                .controls
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
