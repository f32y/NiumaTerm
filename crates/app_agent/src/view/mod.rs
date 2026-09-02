use gpui::prelude::*;
use gpui::{IntoElement, Render};
use gpui_component::button::ButtonVariants as _;
use gpui_component::{ActiveTheme as _, Disableable as _};

use crate::BlurFade;
mod attachments;
mod banners;
mod blocking_overlay;
mod history;
mod last_response;
mod session_state;

#[cfg(test)]
mod tests;

use std::time::Instant;

use gpui::{
    App, ClipboardItem, Context, FocusHandle, FontWeight, MouseButton, MouseUpEvent, Pixels, Point,
    SharedString, WeakEntity, Window, div, px, relative,
};
use gpui_component::button::Button;
use gpui_component::input::{Enter, Escape, IndentInline, Input, MoveDown, MoveUp, Paste};
use gpui_component::modern_menu::ModernMenu;
use gpui_component::{IconName, IconNamed, WindowExt as _, h_flex, v_flex};
use nmt_app_terminal::frame::theme_default_background;
use nmt_config::system::NewlineShortcut;
use nmt_i18n::i18n;

use crate::composer::{CommandFeedbackKind, ComposerAction, PaletteControl, composer_action};
use crate::session::Status;
use crate::settings::{AgentSettings, UI_RADIUS};
// The composer takes the same share of the pane as the transcript column above
// it, so the two edges line up at every window width.
use crate::transcript::transcript_column_margin;
use crate::{AgentPane, AgentPaneEvent, RecentSessionsMode};

/// The composer is the one surface the user types into, so it carries a softer
/// corner than the cards inside the conversation.
const COMPOSER_RADIUS: f32 = 16.0;
/// Diameter of the send/stop control that closes the input line.
const COMPOSER_SEND_BUTTON: f32 = 32.0;
/// Where the card's content starts. The prompt and the settings row under it
/// are the two things read down the card's leading edge, so they stand on the
/// same one: the first glyph of the prompt lines up with the outline of the
/// first pill.
const COMPOSER_EDGE_INSET: f32 = 10.0;
/// The status footer along the bottom edge of the composer card. It reports
/// rather than invites input, so it is set below the chrome size to keep the
/// prompt above it the loudest thing on the card.
pub(super) const COMPOSER_STATUS_PADDING_X: f32 = 14.0;
pub(super) const COMPOSER_STATUS_PADDING_Y: f32 = 6.0;
pub(super) const COMPOSER_STATUS_TEXT_SIZE: f32 = 11.5;

struct StopResponseIcon;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ComposerEnterBehavior {
    InsertNewline,
    Submit,
    ActivateOrSubmit,
}

pub(super) fn composer_enter_behavior(
    shortcut: NewlineShortcut,
    action: &Enter,
) -> ComposerEnterBehavior {
    match (action.secondary, action.shift) {
        (false, false) => ComposerEnterBehavior::ActivateOrSubmit,
        (true, false) if shortcut == NewlineShortcut::CtrlEnter => {
            ComposerEnterBehavior::InsertNewline
        }
        (false, true) if shortcut == NewlineShortcut::ShiftEnter => {
            ComposerEnterBehavior::InsertNewline
        }
        _ => ComposerEnterBehavior::Submit,
    }
}

impl IconNamed for StopResponseIcon {
    fn path(self) -> SharedString {
        "icons/stop.svg".into()
    }
}

impl gpui::EventEmitter<AgentPaneEvent> for AgentPane {}

impl gpui::Focusable for AgentPane {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AgentPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let command_palette = self.render_command_palette(cx);
        let command_feedback = self.palette.visible_feedback().map(|feedback| {
            let (color, label) = match feedback.kind {
                CommandFeedbackKind::Notice => (cx.theme().primary, i18n("agent-feedback-notice")),
                CommandFeedbackKind::Status => {
                    (cx.theme().muted_foreground, i18n("agent-feedback-status"))
                }
                CommandFeedbackKind::Error => (cx.theme().danger, i18n("agent-feedback-error")),
                CommandFeedbackKind::Queued => (cx.theme().warning, i18n("agent-feedback-queued")),
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
        });
        let queued_message = self.render_queued_prompts(cx);
        let session_state = self.render_session_state(cx);

        let approval = self.render_approval_panel(cx);
        let questions = self.render_question_panel(cx);

        let running = composer_action(self.runtime.status) == ComposerAction::Stop;
        let update_suspended = self.runtime.update_suspension.is_some();
        let update_banner = self.render_update_banner(cx);
        let multi_root_notice = self.render_multi_root_notice(cx);
        let update_overlay = self.render_update_overlay(cx);
        let start_overlay = self.render_start_overlay(cx);
        // A branch settled from the backend's answer has no window to reach
        // the composer through, so the prompt it cut in front of is put back
        // here, in the frame that answer asked for.
        self.fill_branch_prompt(window, cx);
        let branch_flow_active = self.branch_flow_holds_composer();
        let branch_flow_working = self.branch_flow_is_working();
        let session_loading = self.history_ui.mode == RecentSessionsMode::Loading;
        let background = if cx
            .global::<AgentSettings>()
            .pane_background_follows_terminal
        {
            gpui::rgb(theme_default_background().rgb_u32()).into()
        } else {
            cx.theme().sidebar
        };

        // Blank tabs expose recent sessions automatically; `/resume` can
        // request the same list after a conversation has started. A count
        // result reserves placeholder rows until the full entries arrive.
        let history_rows = self
            .history_ui
            .pending
            .unwrap_or(self.history_ui.sessions.len());
        let transcript_empty = self.transcript.read(cx).is_empty();
        let history = self
            .history_ui
            .mode
            .is_visible(transcript_empty, history_rows)
            .then(|| self.render_history(background, cx));
        // A list opened over a live conversation is a picker, and the
        // transcript behind it is not what the next click should reach. Blur
        // pushes it back a layer while keeping the tab recognizable as that
        // conversation; a blank tab has nothing to push back.
        let blur_transcript = history.is_some() && !transcript_empty;
        // The ramp is retargeted from the render rather than from the places
        // that open and close the list, so every path in and out of it — the
        // command, Escape, an outside click, resuming a row — animates without
        // each having to remember to.
        let now = Instant::now();
        let blur_target = if blur_transcript { 1.0 } else { 0.0 };
        if self.history_ui.transcript_blur.to != blur_target {
            self.history_ui.transcript_blur = BlurFade {
                from: self.history_ui.transcript_blur.progress(now),
                to: blur_target,
                start: now,
            };
        }
        // Under reduced motion the ramp is still retargeted, so a list opened
        // while it is on and closed after it is off resumes from the blur
        // actually on screen; only the travel to the target is skipped.
        let blur = if cx.global::<AgentSettings>().reduce_motion {
            blur_target
        } else {
            if !self.history_ui.transcript_blur.settled(now) {
                window.request_animation_frame();
            }

            self.history_ui.transcript_blur.progress(now)
        };

        v_flex()
            .size_full()
            .relative()
            // The outer frame matches the window chrome. The Agent surface owns
            // its fill so an opaque main view does not color the rounded frame.
            .bg(background.alpha(cx.global::<AgentSettings>().background_opacity))
            .rounded(UI_RADIUS - px(1.))
            .overflow_hidden()
            .track_focus(&self.focus)
            // Escape force-stops the agent whenever the pane or composer has
            // focus. The input propagates Escape here when the editor did not
            // consume it (inline completion, IME), and transcript clicks focus
            // the pane below. A pending approval is cancelled (deny +
            // interrupt), while a running turn is interrupted directly.
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                // A branch or rewind picker owns Escape ahead of anything
                // under it, and closing one changes nothing else.
                if this.cancel_branch_picker(cx) {
                } else if this.pending_approval.is_some() {
                    this.respond_approval("cancel", cx);
                } else if this.pending_questions.is_some() {
                    // Dismissing questions is not cancelling the turn: the
                    // model is told to assume and continue, so Escape here
                    // deliberately does not interrupt.
                    this.respond_questions(false, cx);
                } else if this.runtime.status == Status::Running {
                    this.interrupt_from_ui(window, cx);
                }
            }))
            // The agent tab is a terminal surface stand-in, so it overrides the
            // chrome's UI font with its own configured font (Settings → Agent
            // Font), same as the terminal pane does with the terminal font.
            .font(cx.global::<AgentSettings>().font())
            .text_size(px(cx.global::<AgentSettings>().font_size))
            .children(multi_root_notice)
            .children(update_banner)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    // Selectable transcript text claims focus during mouse-down
                    // dispatch, so restore the composer on release. Escape then
                    // reaches the pane-level interrupt handler through the input.
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseUpEvent, window, cx| {
                            this.focus(window, cx);
                            let pane = cx.entity().downgrade();
                            // Kept as the fallback anchor for a selection whose
                            // rect cannot be resolved, so the menu still opens
                            // somewhere the pointer just was.
                            let released_at = event.position;
                            window.on_next_frame(move |window, cx| {
                                Self::show_selected_text_menu(pane, released_at, window, cx);
                            });
                            cx.notify();
                        }),
                    )
                    .relative()
                    .child(self.transcript.clone())
                    .when(blur > 0.0, |this| {
                        this.child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full()
                                // Swallows clicks aimed at the transcript; the
                                // list's outside-click handler still sees them
                                // and dismisses itself. Only while the list is
                                // up: the fade out outlives it, and a layer
                                // still eating clicks after it closed would
                                // read as the tab having hung.
                                .when(blur_transcript, |this| this.occlude())
                                .backdrop_blur(px(16. * blur))
                                .bg(cx.theme().background.opacity(0.25 * blur)),
                        )
                    }),
            )
            .child({
                // Composer area: auxiliary strips sit outside the bordered,
                // shadowed shell on a deeper surface. History is absolutely
                // anchored above the shell because it only exists while the
                // transcript is empty; loading it must never participate in
                // composer height calculation. Both strips are painted before
                // the shell, whose edge and shadow keep them visibly tucked
                // behind the input card.
                div()
                    .w_full()
                    .px(relative(transcript_column_margin()))
                    .pb_3()
                    .pt_1()
                    .child(
                        div()
                            .w_full()
                            .relative()
                            .children(history.map(|history| {
                                div()
                                    .absolute()
                                    .left_0()
                                    .right_0()
                                    .bottom(relative(1.))
                                    .mb(px(-14.))
                                    .child(history)
                            }))
                            .child(
                                v_flex()
                                    .w_full()
                                    .rounded(px(COMPOSER_RADIUS))
                                    .overflow_hidden()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(cx.theme().popover)
                                    .shadow_md()
                                    .children(approval)
                                    .children(questions)
                                    .children(command_feedback)
                                    .children(session_state)
                                    .children(queued_message)
                                    .children(self.render_attachments(cx))
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .px(px(COMPOSER_EDGE_INSET))
                                            .pt_3()
                                            .pb_1()
                                            // GPUI resolves these keystrokes
                                            // into Input actions before raw
                                            // key listeners run. Capturing
                                            // the actions lets the palette
                                            // own navigation while visible;
                                            // the handler propagates them
                                            // unchanged when it is closed.
                                            // The composer's own paste inserts
                                            // text; an image on the clipboard has
                                            // to be taken before it gets there.
                                            .capture_action(cx.listener(
                                                |this, _: &Paste, window, cx| {
                                                    if this.paste_image(window, cx) {
                                                        cx.stop_propagation();
                                                    }
                                                },
                                            ))
                                            .capture_action(cx.listener(
                                                |this, _: &MoveUp, window, cx| {
                                                    this.handle_palette_control(
                                                        PaletteControl::Previous,
                                                        window,
                                                        cx,
                                                    )
                                                },
                                            ))
                                            .capture_action(cx.listener(
                                                |this, _: &MoveDown, window, cx| {
                                                    this.handle_palette_control(
                                                        PaletteControl::Next,
                                                        window,
                                                        cx,
                                                    )
                                                },
                                            ))
                                            .capture_action(cx.listener(
                                                |this, action: &Enter, window, cx| {
                                                    match composer_enter_behavior(
                                                        cx.global::<AgentSettings>()
                                                            .newline_shortcut,
                                                        action,
                                                    ) {
                                                        ComposerEnterBehavior::InsertNewline => {
                                                            this.input.update(cx, |input, cx| {
                                                                input.replace("\n", window, cx);
                                                            });
                                                            cx.stop_propagation();
                                                        }
                                                        ComposerEnterBehavior::Submit => {
                                                            this.send_user_message(window, cx);
                                                            cx.stop_propagation();
                                                        }
                                                        ComposerEnterBehavior::ActivateOrSubmit => {
                                                            this.handle_palette_control(
                                                                PaletteControl::Activate,
                                                                window,
                                                                cx,
                                                            )
                                                        }
                                                    }
                                                },
                                            ))
                                            .capture_action(cx.listener(
                                                |this, _: &IndentInline, window, cx| {
                                                    this.handle_palette_control(
                                                        PaletteControl::Complete,
                                                        window,
                                                        cx,
                                                    )
                                                },
                                            ))
                                            .capture_action(cx.listener(
                                                |this, _: &Escape, window, cx| {
                                                    this.handle_palette_control(
                                                        PaletteControl::Dismiss,
                                                        window,
                                                        cx,
                                                    )
                                                },
                                            ))
                                            // The prompt editor reads larger than the
                                            // chrome around it (t3code uses 16px over
                                            // a 14px UI); +2 keeps that ratio at any
                                            // configured agent font size.
                                            .text_size(px(
                                                cx.global::<AgentSettings>().font_size + 2.0
                                            ))
                                            .child(div().flex_1().min_w_0().child(
                                                Input::new(&self.input).appearance(false).disabled(
                                                    branch_flow_working
                                                        || session_loading
                                                        || update_suspended,
                                                ),
                                            )),
                                    )
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .px(px(COMPOSER_EDGE_INSET))
                                            .pb_2()
                                            .pt_0p5()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .child(self.controls.render_row(self.kind, cx)),
                                            )
                                            .children(self.render_last_response(cx))
                                            // Send stands at the card's trailing
                                            // corner, past the settings it is
                                            // qualified by: those say what the next
                                            // message is sent as, and this is the
                                            // one control that sends it, so it is
                                            // the last thing the eye reaches on its
                                            // way out of the card. Stop replaces
                                            // Send in place while a turn runs.
                                            .child(if running {
                                                Button::new("agent-send")
                                                    .primary()
                                                    .size(px(COMPOSER_SEND_BUTTON))
                                                    .rounded_full()
                                                    .icon(StopResponseIcon)
                                                    .tooltip(i18n("agent-action-stop-response"))
                                                    .aria_label(i18n("agent-action-stop-response"))
                                                    .on_click(cx.listener(|this, _, window, cx| {
                                                        this.interrupt_from_ui(window, cx)
                                                    }))
                                            } else {
                                                Button::new("agent-send")
                                                    .primary()
                                                    .disabled(
                                                        branch_flow_active
                                                            || session_loading
                                                            || update_suspended,
                                                    )
                                                    .size(px(COMPOSER_SEND_BUTTON))
                                                    .rounded_full()
                                                    .icon(IconName::ArrowUp)
                                                    .tooltip(i18n("agent-action-send-message"))
                                                    .aria_label(i18n("agent-action-send-message"))
                                                    .on_click(cx.listener(|this, _, window, cx| {
                                                        this.send_user_message(window, cx)
                                                    }))
                                            }),
                                    ),
                            )
                            // The status footer reads out what the session has
                            // spent so far, which is context for the message
                            // rather than part of composing it. It sits under
                            // the card on the pane's own surface, so the card's
                            // edge still ends at the input it encloses.
                            .child(self.render_composer_status(cx))
                            .children(command_palette.map(|palette| {
                                div()
                                    .absolute()
                                    .left_0()
                                    .right_0()
                                    .bottom(relative(1.))
                                    .mb_2()
                                    .occlude()
                                    .child(palette)
                            })),
                    )
            })
            // Painted last so it sits over the transcript and the composer.
            .children(update_overlay)
            .children(start_overlay)
    }
}

impl AgentPane {
    fn show_selected_text_menu(
        pane: WeakEntity<Self>,
        released_at: Point<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let selected_text = window.selected_text(cx).trim().to_string();
        if selected_text.is_empty() {
            return;
        }

        // Anchored on the selection rather than the pointer, and opened above
        // it, so the text the two actions operate on stays visible while the
        // menu is up. The rect is the union of the selected line boxes, so its
        // top-left is above and left of every selected line.
        let anchor = window
            .selected_text_bounds(cx)
            .map_or(released_at, |bounds| bounds.origin);

        let copy_text = selected_text.clone();
        ModernMenu::new()
            // A selection menu offers two actions that are recognised by icon, so
            // the command row reaches them in one horizontal band instead of a
            // stack of labelled rows the pointer has to travel down.
            .commands(|menu| {
                menu.item(i18n("agent-transcript-copy"), move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                })
                .icon(IconName::Copy)
                .item(i18n("agent-transcript-quote"), move |window, cx| {
                    let selected_text = selected_text.clone();
                    let _ = pane.update(cx, |pane, cx| {
                        pane.add_response_annotation(selected_text, window, cx);
                    });
                })
                .icon(IconName::TextSelect)
            })
            .show_above(anchor, window, cx);
    }
}
