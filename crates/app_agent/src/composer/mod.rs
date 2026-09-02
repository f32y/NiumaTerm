use gpui::prelude::*;
use gpui_component::ActiveTheme as _;
use gpui_component::button::ButtonVariants as _;

pub(crate) mod attachments;
mod branch;
pub(super) mod images;
mod palette;
mod response_annotations;
mod slash;

pub(super) use crate::composer::branch::BranchFlow;
#[cfg(test)]
pub(super) use crate::composer::branch::fork::checkpoint_at_depth;
pub(super) use crate::composer::branch::fork::{ForkState, PromptTarget};
#[cfg(test)]
use crate::composer::branch::rewind::{
    FileRestoreNext, file_restore_next, rewind_blocks_submission,
};
pub(super) use crate::composer::branch::rewind::{RewindAction, RewindState};
pub(super) use crate::composer::palette::{
    PALETTE_MAX_HEIGHT, PaletteAction, PaletteControl, PaletteModel, PaletteRow,
};
pub(crate) use crate::composer::response_annotations::{
    annotation_count_label, parse_annotated_prompt, prompt_with_response_annotations,
    visible_prompt,
};
#[cfg(test)]
mod tests;

use std::time::Duration;

use gpui::{Context, SharedString, Window};
use gpui_component::button::Button;
use gpui_component::dialog::{DIALOG_BUTTON_MIN_WIDTH, DialogClose, DialogFooter};
use gpui_component::{WindowExt, v_flex};
use nmt_i18n::i18n;

use crate::commands::{parse_slash_command, reconcile_skill_binding, validate_skill_binding};
use crate::session::Status;
use crate::transcript::last_response_label;
use crate::{AgentPane, RecentSessionsMode, SlashPalette};

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
    /// Redrawn on every frame the composer paints while the message is up, so
    /// it is stored in the form the view hands to `child` rather than copied
    /// into one each time.
    pub(super) message: SharedString,
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

impl SlashPalette {
    /// Stand down for text the composer did not type. A recalled entry is a
    /// whole message, so a skill bound to what was there no longer applies and
    /// the palette must not reopen on the leading `/` the entry may carry.
    pub(crate) fn reset_for_recall(&mut self) {
        self.skill_binding = None;
        self.dismissed = true;
        self.selected = 0;
    }

    /// Show a one-line result above the composer, replacing whatever was
    /// there.
    pub(crate) fn set_feedback(
        &mut self,
        kind: CommandFeedbackKind,
        message: impl Into<SharedString>,
        cx: &mut Context<AgentPane>,
    ) {
        self.feedback_seq += 1;
        let seq = self.feedback_seq;
        self.feedback = Some(CommandFeedback {
            kind,
            message: message.into(),
        });
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
    pub(crate) fn visible_feedback(&self) -> Option<&CommandFeedback> {
        self.feedback
            .as_ref()
            .filter(|feedback| feedback_is_current(feedback.kind, self.command_queue.is_empty()))
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
                .turn
                .last_response_at()
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(u64::from(minutes) * 60))
    }

    /// Ask before paying for a cold prompt cache. Cancelling leaves the text
    /// in the composer, so the decision costs nothing to reverse.
    fn confirm_send_after_cache_expiry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let idle = self
            .turn
            .last_response_at()
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
            self.palette.set_feedback(
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
                    self.palette
                        .set_feedback(CommandFeedbackKind::Error, message, cx);
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

    pub(super) fn is_command_busy(&self) -> bool {
        self.runtime.status == Status::Running
            || self.palette.awaiting_command_turn
            || self.history_ui.mode == RecentSessionsMode::Loading
            || self.branch_flow_holds_composer()
    }
}

impl AgentPane {}
