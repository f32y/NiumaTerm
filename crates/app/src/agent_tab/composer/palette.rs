use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, FontWeight, Pixels, ScrollHandle, SharedString, div, px};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};
use nmt_agent::chat::{ForkCheckpoint, SkillInfo, SkillReference, SlashCommandInfo};
use nmt_agent::claude_code::sessions;
use nmt_agent::session::commands::CommandQueue;
use rust_i18n::t;

use crate::agent_tab::AgentPane;
use crate::agent_tab::composer::{CommandFeedback, CommandFeedbackKind, RewindAction};
use crate::agent_tab::settings::UI_RADIUS;

/// Tallest the palette grows before its own rows scroll: nine rows and the
/// note under them. The transcript reads this as the height the picker covers
/// when it floats over the bottom of the pane.
pub(crate) const PALETTE_MAX_HEIGHT: Pixels = px(9. * 48. + 36.);

#[derive(Clone)]
pub(crate) enum PaletteAction {
    Command(SlashCommandInfo),
    Choice { command: String, value: String },
    Skill(SkillInfo),
    RewindCheckpoint(sessions::ClaudeCheckpoint),
    RewindAction(RewindAction),
    ForkCheckpoint(ForkCheckpoint),
    ForkCancel,
}

/// One drawn palette row. The text is `SharedString` because the model is
/// rebuilt on every frame the palette paints: catalog text taken straight from
/// the translation catalogs borrows instead of copying, and text that is
/// composed still reaches `child` without a second copy.
#[derive(Clone)]
pub(crate) struct PaletteRow {
    pub(crate) label: SharedString,
    pub(crate) description: SharedString,
    pub(crate) hint: Option<SharedString>,
    pub(crate) disabled_reason: Option<SharedString>,
    pub(crate) action: PaletteAction,
}

pub(crate) struct PaletteModel {
    pub(crate) rows: Vec<PaletteRow>,
    pub(crate) note: Option<SharedString>,
}

/// The merged `/` catalog, held so it is not rebuilt from the local, adapter,
/// and provider lists on every frame the palette paints. `language` is part of
/// the key because local entries carry translated descriptions and the user can
/// switch language while a pane is open.
pub(crate) struct CachedCatalog {
    pub(crate) language: String,
    pub(crate) epoch: u64,
    pub(crate) commands: Rc<[SlashCommandInfo]>,
}

/// Slash-command palette, skill picker, and pending-command state.
#[derive(Default)]
pub(crate) struct SlashPalette {
    /// Merged commands are invalidated by provider discovery, language, or epoch changes.
    pub(crate) catalog: Option<CachedCatalog>,

    /// Exact picker identity retained while the composer keeps its `$name`
    /// token. It is validated against the session catalog before every send.
    pub(crate) skill_binding: Option<SkillReference>,

    pub(crate) selected: usize,
    pub(crate) dismissed: bool,
    pub(crate) scroll: ScrollHandle,
    pub(crate) feedback: Option<CommandFeedback>,

    /// Order of the latest feedback shown. A delayed dismissal compares
    /// against it so it can only retire the message it was started for.
    pub(crate) feedback_seq: u64,
}

impl SlashPalette {
    pub(crate) fn reset_discovery(&mut self) {
        self.catalog = None;
        self.selected = 0;
        self.dismissed = false;
    }

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
    pub(crate) fn visible_feedback(&self, commands: &CommandQueue) -> Option<&CommandFeedback> {
        self.feedback
            .as_ref()
            .filter(|feedback| feedback_is_current(feedback.kind, commands.queue.is_empty()))
    }

    /// The palette listing `model`'s rows. `hover_selects` moves the highlight
    /// with the pointer, which only a picker of branch points wants.
    pub(crate) fn render(
        &self,
        model: PaletteModel,
        hover_selects: bool,
        cx: &mut Context<AgentPane>,
    ) -> AnyElement {
        let selected = self.selected.min(model.rows.len().saturating_sub(1));

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
                    .when(hover_selects && !disabled, |this| {
                        this.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            if *hovered {
                                this.hover_palette_index(index, cx);
                            }
                        }))
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

        v_flex()
            .id("agent-slash-command-palette")
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss_command_palette(cx)))
            .w_full()
            .max_h(PALETTE_MAX_HEIGHT)
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p_1()
            .rounded(UI_RADIUS)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .shadow_lg()
            .children(rows)
            .children(note)
            .into_any_element()
    }

    /// The line inside the composer card reporting the latest command result
    /// still worth showing against `commands`.
    pub(crate) fn render_feedback(
        &self,
        commands: &CommandQueue,
        cx: &App,
    ) -> Option<impl IntoElement + use<>> {
        self.visible_feedback(commands).map(|feedback| {
            let (color, label) = match feedback.kind {
                CommandFeedbackKind::Notice => (cx.theme().primary, t!("agent-feedback-notice")),
                CommandFeedbackKind::Status => {
                    (cx.theme().muted_foreground, t!("agent-feedback-status"))
                }
                CommandFeedbackKind::Error => (cx.theme().danger, t!("agent-feedback-error")),
                CommandFeedbackKind::Queued => (cx.theme().warning, t!("agent-feedback-queued")),
            };

            h_flex()
                .w_full()
                .gap_2()
                .px_3()
                .pb_2()
                .text_xs()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(color)
                        .child(label),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_color(cx.theme().muted_foreground)
                        .child(feedback.message.clone()),
                )
        })
    }
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
pub(super) fn feedback_is_current(kind: CommandFeedbackKind, queue_is_empty: bool) -> bool {
    !(kind == CommandFeedbackKind::Queued && queue_is_empty)
}

/// Whether a message is a passing acknowledgement rather than something the
/// user still has to act on. An error stays until it is read, and a queued
/// list describes work still waiting rather than work already accepted.
pub(super) fn feedback_is_transient(kind: CommandFeedbackKind) -> bool {
    match kind {
        CommandFeedbackKind::Notice => true,
        CommandFeedbackKind::Status | CommandFeedbackKind::Error | CommandFeedbackKind::Queued => {
            false
        }
    }
}
