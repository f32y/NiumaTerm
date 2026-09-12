//! A slash line from the moment it is submitted to the moment it runs.
//!
//! A `/` line steers the session rather than continuing the conversation, and
//! which side owns a given command differs: some the pane answers itself, the
//! rest go to the harness and come back as a result. Routing is what decides
//! between them, and the queue is what keeps a command from overtaking a turn.

use std::rc::Rc;

use gpui::{Context, SharedString, Window};
use nmt_agent::catalog::adapter_commands;
use nmt_agent::chat::{
    SkillInfo, SlashCommandArguments, SlashCommandInfo, SlashCommandOutcome, SlashCommandRunPolicy,
};
use nmt_agent::claude_code::stream_json;
use nmt_agent::codex::app_server;
use nmt_agent::session::commands::CommandAdmission;
use rust_i18n::t;

use crate::agent_tab::capabilities::AgentCapabilities as _;
use crate::agent_tab::commands::{
    local_commands, merge_catalog, parse_slash_command, resolve_choice, setting_value_label,
};
use crate::agent_tab::composer::{CommandFeedbackKind, PendingSlashCommand};
use crate::agent_tab::profile::AgentKind;
use crate::agent_tab::session::{Backend, Status};
use crate::agent_tab::{AgentPane, CachedCatalog, RecentSessionsMode};

impl AgentPane {
    pub(in crate::agent_tab) fn submit_current_slash(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        if !self.binding.is_current() {
            return false;
        }

        let Some(parsed) = parse_slash_command(input) else {
            return false;
        };

        if parsed.name.is_empty() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-composer-choose-command").to_string(),
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
                t!("agent-composer-unknown-command", name = &parsed.name).into_owned(),
                cx,
            );

            return false;
        };

        // `/skills` owns a picker stage. A selected row rewrites the
        // composer to `$name`; the slash input itself is never a provider
        // command or an ordinary user turn.
        if command.arguments == SlashCommandArguments::Skills {
            let message = match self.palette.skill_catalog.as_ref() {
                None => t!("agent-composer-skill-discovery-loading-period").to_string(),

                Some(catalog) if catalog.skills.is_empty() && !catalog.errors.is_empty() => {
                    catalog.errors[0].clone()
                }

                Some(catalog) if catalog.skills.is_empty() => {
                    t!("agent-composer-no-skills-period").to_string()
                }

                Some(_) => t!("agent-composer-choose-skill").to_string(),
            };

            self.palette
                .set_feedback(CommandFeedbackKind::Error, message, cx);

            return false;
        }

        if command.arguments == SlashCommandArguments::None && !parsed.arguments.trim().is_empty() {
            self.palette.set_feedback(
                CommandFeedbackKind::Error,
                t!("agent-composer-command-no-arguments", name = &command.name).into_owned(),
                cx,
            );

            return false;
        }

        if command.arguments == SlashCommandArguments::Choices {
            if parsed.arguments.trim().is_empty() {
                self.palette.set_feedback(
                    CommandFeedbackKind::Error,
                    t!("agent-composer-choose-value", name = &command.name).into_owned(),
                    cx,
                );

                return false;
            }

            let choices = self.command_choices(&command.name);

            match resolve_choice(&parsed.arguments, &choices) {
                Ok(value) if command.name == "model" => {
                    self.session.borrow_mut().controls.settings.model = Some(value.clone());

                    self.controls.remember_defaults(
                        &self.session.borrow().controls,
                        self.kind,
                        &self.profile,
                        cx,
                    );

                    self.palette.set_feedback(
                        CommandFeedbackKind::Notice,
                        t!("agent-composer-model-set", value = &value).into_owned(),
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
                    self.session.borrow_mut().controls.settings.approval = Some(value.clone());

                    self.controls.remember_defaults(
                        &self.session.borrow().controls,
                        self.kind,
                        &self.profile,
                        cx,
                    );

                    self.palette.set_feedback(
                        CommandFeedbackKind::Notice,
                        t!(
                            "agent-composer-permissions-set",
                            value = &setting_value_label(&value)
                        )
                        .into_owned(),
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
                        t!("agent-composer-command-idle-only", name = &command.name).into_owned(),
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
            let admission = self
                .session
                .borrow_mut()
                .commands
                .while_busy(command, policy);

            return match admission {
                CommandAdmission::Queued { name, count } => {
                    self.palette.set_feedback(
                        CommandFeedbackKind::Queued,
                        t!(
                            if count == 1 {
                                "agent-composer-command-queued-one"
                            } else {
                                "agent-composer-command-queued-many"
                            },
                            name = &name,
                            count = count
                        )
                        .into_owned(),
                        cx,
                    );

                    true
                }

                CommandAdmission::Busy { name } => {
                    self.palette.set_feedback(
                        CommandFeedbackKind::Error,
                        t!("agent-composer-command-idle-only", name = &name).into_owned(),
                        cx,
                    );

                    false
                }

                CommandAdmission::Execute(command) => self.execute_backend_command(command, cx),
            };
        }

        self.execute_backend_command(command, cx)
    }

    pub(in crate::agent_tab) fn execute_backend_command(
        &mut self,
        command: PendingSlashCommand,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.binding.is_current() {
            return false;
        }

        let outcome = self.session.borrow_mut().execute_command(&command);

        match outcome {
            SlashCommandOutcome::Accepted => {
                self.history_ui.mode = RecentSessionsMode::Hidden;

                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    t!("agent-composer-command-starting", name = &command.name).into_owned(),
                    cx,
                );

                true
            }

            SlashCommandOutcome::Completed { message } => {
                self.palette.set_feedback(
                    CommandFeedbackKind::Notice,
                    message.unwrap_or_else(|| {
                        t!("agent-session-command-completed", name = &command.name).into_owned()
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
                    t!("agent-session-still-starting", name = self.kind.display()).into_owned(),
                    cx,
                );

                false
            }
        }
    }

    pub(in crate::agent_tab) fn run_next_queued_command(&mut self, cx: &mut Context<Self>) {
        if self.presenting_session_effect {
            return;
        }

        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, cx| host.advance_commands(cx));
        }
    }

    pub(super) fn show_status(&mut self, cx: &mut Context<Self>) {
        let status = match self.session.borrow().runtime.status() {
            Status::Starting => t!("agent-composer-status-starting"),
            Status::Idle => t!("agent-composer-status-idle"),
            Status::Running => t!("agent-composer-status-running"),
            Status::Exited => t!("agent-composer-status-exited"),
        };

        let mut fields = vec![
            t!(
                "agent-composer-status-field",
                name = t!("agent-composer-status-backend"),
                value = self.kind.display()
            )
            .into_owned(),
            t!(
                "agent-composer-status-field",
                name = t!("agent-composer-status-label"),
                value = status
            )
            .into_owned(),
        ];

        for (name, value) in [
            (
                t!("agent-setting-model"),
                self.session.borrow().controls.settings.model.as_deref(),
            ),
            (
                t!("agent-setting-permissions"),
                self.session.borrow().controls.settings.approval.as_deref(),
            ),
            (
                t!("agent-setting-sandbox"),
                self.session.borrow().controls.settings.sandbox.as_deref(),
            ),
            (
                t!("agent-setting-effort"),
                self.session.borrow().controls.settings.effort.as_deref(),
            ),
            (
                t!("agent-setting-tier"),
                self.session.borrow().controls.settings.tier.as_deref(),
            ),
        ] {
            if let Some(value) = value {
                fields.push(
                    t!("agent-composer-status-field", name = name, value = value).into_owned(),
                );
            }
        }

        if !self.session.borrow().commands.queue.is_empty() {
            fields.push(
                t!(
                    "agent-composer-status-field",
                    name = t!("agent-composer-status-queued"),
                    value = self.session.borrow().commands.queue.len()
                )
                .into_owned(),
            );
        }

        // Answering /status is information the user asked for, so it holds
        // rather than fading out from under them.
        self.palette
            .set_feedback(CommandFeedbackKind::Status, fields.join(" · "), cx);
    }

    pub(super) fn skill_disabled_reason(&self, skill: &SkillInfo) -> Option<SharedString> {
        if !skill.enabled {
            Some(SharedString::from(t!("agent-composer-disabled-by-codex")))
        } else {
            // A skill is invoked through the harness, so it needs a session
            // that has finished starting and has not ended.
            match self.session.borrow().runtime.status() {
                Status::Starting => Some(SharedString::from(t!("agent-composer-agent-starting"))),
                Status::Exited => Some(SharedString::from(t!("agent-composer-agent-exited"))),
                _ => None,
            }
        }
    }

    pub(in crate::agent_tab) fn command_catalog(&mut self) -> Rc<[SlashCommandInfo]> {
        let language = rust_i18n::locale();

        if let Some(cached) = self
            .palette
            .catalog
            .as_ref()
            .filter(|cached| cached.language == *language)
        {
            return cached.commands.clone();
        }

        let adapter = self
            .session
            .borrow()
            .runtime
            .backend()
            .map(Backend::adapter_commands)
            .unwrap_or_else(|| adapter_commands(self.kind));

        let commands: Rc<[SlashCommandInfo]> = merge_catalog(
            local_commands(),
            adapter,
            self.palette.provider_commands.clone(),
        )
        .into();

        self.palette.catalog = Some(CachedCatalog {
            language: language.to_string(),
            commands: commands.clone(),
        });

        commands
    }

    pub(super) fn command_choices(&self, command: &str) -> Vec<(String, String)> {
        match command {
            "model" => self
                .session
                .borrow()
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
