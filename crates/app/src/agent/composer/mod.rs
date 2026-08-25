pub(in crate::agent) mod attachments;
mod fork;
mod palette;
mod response_annotations;
mod rewind;

#[cfg(test)]
pub(super) use crate::agent::composer::fork::checkpoint_at_depth;
pub(super) use crate::agent::composer::fork::{ForkFlow, ForkState, PromptTarget};
pub(super) use crate::agent::composer::palette::{
    PALETTE_MAX_HEIGHT, PaletteAction, PaletteControl, PaletteModel, PaletteRow,
};
pub(in crate::agent) use crate::agent::composer::response_annotations::{
    annotation_count_label, parse_annotated_prompt, prompt_with_response_annotations,
    visible_prompt,
};
#[cfg(test)]
use crate::agent::composer::rewind::{
    FileRestoreNext, file_restore_next, rewind_blocks_submission,
};
pub(super) use crate::agent::composer::rewind::{RewindAction, RewindState};
#[cfg(test)]
mod tests;

use std::fs;
use std::path::Path;

use gpui::{ClipboardEntry, Image, ImageFormat};
use gpui_component::WindowExt;
use gpui_component::dialog::{DIALOG_BUTTON_MIN_WIDTH, DialogClose, DialogFooter};
use nmt_i18n::i18n;

use crate::agent::composer::attachments::{AttachError, MAX_ATTACHMENTS};
use crate::agent::transcript::last_response_label;
use crate::agent::*;

#[derive(Clone)]
pub(super) struct PendingSlashCommand {
    name: String,
    arguments: String,
}

impl PendingSlashCommand {
    /// One command a control runs directly, without the composer's parsing
    /// stage: the caller already knows the name and the value it is passing.
    pub(super) fn new(name: &str, arguments: String) -> Self {
        Self {
            name: name.to_string(),
            arguments,
        }
    }
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
    /// Send what the composer holds, warning first when the conversation has
    /// been idle long enough for the provider's prompt cache to have expired.
    pub(super) fn send_user_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Slash lines steer the session (`/new`, `/model`, `/status`) rather
        // than continue the conversation, so a warning about what the next
        // answer costs would fire in front of commands that ask for none.
        let text = self.input.read(cx).text().to_string();
        if !text.trim().is_empty()
            && parse_slash_command(&text).is_none()
            && self.prompt_cache_may_have_expired(cx)
        {
            self.confirm_send_after_cache_expiry(window, cx);
            return;
        }

        self.send_user_message_now(window, cx);
    }

    /// Whether the idle span since the agent last answered has passed the
    /// profile's warning threshold. A running turn is still writing into the
    /// live cache, so a mid-turn steer never counts as a cold start.
    fn prompt_cache_may_have_expired(&self, cx: &Context<Self>) -> bool {
        let minutes = self.profile.cache_warn_minutes;
        minutes > 0
            && !self.transcript.read(cx).is_working()
            && self
                .last_response_at
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(u64::from(minutes) * 60))
    }

    /// Ask before paying for a cold prompt cache. Cancelling leaves the text
    /// in the composer, so the decision costs nothing to reverse.
    fn confirm_send_after_cache_expiry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let idle = self
            .last_response_at
            .map(|at| last_response_label(at.elapsed().as_secs()))
            .unwrap_or_default();
        let pane = cx.entity();

        window.open_dialog(cx, move |dialog, _, _| {
            let pane = pane.clone();
            let idle = idle.clone();

            dialog
                .title(i18n("agent-cache-warning-title"))
                .overlay_closable(false)
                .content(move |content, _, cx| {
                    content.child(
                        v_flex()
                            .gap_1()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(idle.clone())
                            .child(i18n("agent-cache-warning-message")),
                    )
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("agent-cache-warning-send")
                                .min_w(DIALOG_BUTTON_MIN_WIDTH)
                                .label(i18n("agent-cache-warning-send"))
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    pane.update(cx, |pane, cx| {
                                        pane.send_user_message_now(window, cx)
                                    });
                                }),
                        )
                        .child(
                            DialogClose::new().child(
                                Button::new("agent-cache-warning-cancel")
                                    .min_w(DIALOG_BUTTON_MIN_WIDTH)
                                    .primary()
                                    .label(i18n("agent-cache-warning-cancel")),
                            ),
                        ),
                )
        });
    }

    fn send_user_message_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.branch_flow_holds_composer() {
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
            // Where a skill is invoked by writing its name into the prompt, a
            // slash line naming one is a message the harness expands, so
            // refusing it as an unknown command would block the only way to
            // reach a skill at all.
            if self.kind.caps().slash_skills_are_prompts && self.names_a_skill(&parsed.name) {
                return self.send_text(input.to_string(), cx);
            }

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
    pub(in crate::agent) fn visible_command_feedback(&self) -> Option<&CommandFeedback> {
        self.palette.feedback.as_ref().filter(|feedback| {
            feedback_is_current(feedback.kind, self.palette.command_queue.is_empty())
        })
    }

    pub(super) fn is_command_busy(&self) -> bool {
        self.status == Status::Running
            || self.palette.awaiting_command_turn
            || self.history_ui.mode == RecentSessionsMode::Loading
            || self.branch_flow_holds_composer()
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
                AgentKind::DeepSeek => deepseek::Session::adapter_commands(),
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

impl AgentPane {
    /// Take a pasted image into the pending message, reporting whether the
    /// paste was consumed. A paste this leaves alone falls through to the
    /// composer's own text handling, which is what a clipboard holding text
    /// should get.
    pub(in crate::agent) fn paste_image(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // An image reaches the clipboard two ways: as pixels, from a capture
        // tool or a browser, and as a file, from a file manager. Both are the
        // same gesture to the person doing it.
        let Some(image) = cx
            .read_from_clipboard()
            .into_iter()
            .flat_map(|item| item.into_entries())
            .find_map(|entry| match entry {
                ClipboardEntry::Image(image) => Some(image),
                ClipboardEntry::ExternalPaths(paths) => {
                    paths.paths().iter().find_map(|path| image_file(path))
                }
                ClipboardEntry::String(_) => None,
            })
        else {
            return false;
        };

        if !self.kind.caps().image_input {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                i18n("agent-composer-images-unsupported").replace("{name}", self.kind.display()),
                cx,
            );
            return true;
        }

        match self.attachments.attach(&image) {
            Ok(placeholder) => {
                self.input.update(cx, |input, cx| {
                    let preceding = input.text().chars_at(input.cursor()).prev();
                    input.insert(spaced_placeholder(preceding, &placeholder), window, cx);
                });
                cx.notify();
                true
            }
            Err(AttachError::Full) => {
                self.set_command_feedback(
                    CommandFeedbackKind::Error,
                    i18n("agent-composer-images-full")
                        .replace("{count}", &MAX_ATTACHMENTS.to_string()),
                    cx,
                );
                true
            }
            // Something on the clipboard claimed to be an image and was not.
            // Falling through lets the composer paste whatever text is there.
            Err(AttachError::Undecodable) => false,
        }
    }

    /// Drop the attachment at `index` by deleting its placeholder, then let
    /// reconciliation renumber what is left. Removal and a hand-edited
    /// deletion therefore take the same path.
    pub(in crate::agent) fn remove_attachment(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(placeholder) = self.attachments.placeholder_at(index) else {
            return;
        };

        let text = self.input.read(cx).text().to_string();
        let without = text.replace(placeholder, "");

        self.input
            .update(cx, |input, cx| input.set_value(without.clone(), window, cx));
        self.sync_attachments(&without, window, cx);
    }

    /// Bring the attachment list back in line with the composer text. The text
    /// is the record of which images the message still carries, so this runs
    /// after every edit that could have changed its placeholders.
    pub(in crate::agent) fn sync_attachments(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.attachments.is_empty() {
            return;
        }

        if let Some(renumbered) = self.attachments.reconcile(text) {
            self.input
                .update(cx, |input, cx| input.set_value(renumbered, window, cx));
        }

        cx.notify();
    }
}

/// A copied file read as an image, or `None` for anything that is not one.
/// Only the extension is trusted to decide whether reading is worth it; the
/// decode decides whether it was an image.
/// The placeholder as it is written into the composer. A space on each side
/// keeps it a word of its own, so the prompt around it does not run into the
/// marker. The leading one is dropped where there is nothing to separate it
/// from: the start of the text, or whitespace the caret already sits after.
fn spaced_placeholder(preceding: Option<char>, placeholder: &str) -> String {
    match preceding {
        Some(character) if !character.is_whitespace() => format!(" {placeholder} "),
        _ => format!("{placeholder} "),
    }
}

fn image_file(path: &Path) -> Option<Image> {
    let format = match path
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "webp" => ImageFormat::Webp,
        "gif" => ImageFormat::Gif,
        "bmp" => ImageFormat::Bmp,
        _ => return None,
    };

    Some(Image::from_bytes(format, fs::read(path).ok()?))
}
