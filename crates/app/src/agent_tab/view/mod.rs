use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClipboardItem, Context, FocusHandle, FontWeight, IntoElement, MouseButton,
    MouseUpEvent, Pixels, Point, Render, SharedString, WeakEntity, Window, div, px, relative,
};
use gpui_base::TextSelection;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Enter, Escape, IndentInline, MoveDown, MoveUp, Paste, Textarea};
use gpui_component::modern_menu::ModernMenu;
use gpui_component::tooltip::Tooltip;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, IconNamed, WindowExt as _, h_flex, v_flex,
};
use nmt_config::system::NewlineShortcut;
use rust_i18n::t;

use crate::agent_tab::composer::{CommandFeedbackKind, ComposerAction, PaletteControl};
use crate::agent_tab::fade::FrostedLayer;
use crate::agent_tab::session::Status;
use crate::agent_tab::settings::{AgentSettings, UI_RADIUS};
use crate::agent_tab::transcript::{LAST_RESPONSE_LIMIT, last_response_label, transcript_column};
use crate::agent_tab::{AgentPane, AgentPaneEvent, RecentSessionsMode};

mod banners;
mod history;
pub(in crate::agent_tab) mod session_state;

#[cfg(test)]
mod tests;

// The composer sits in the same column as the transcript above it, so the
// two edges line up at every window width.

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

        let command_feedback = self
            .palette
            .visible_feedback(&self.session.borrow().commands)
            .map(|feedback| {
                let (color, label) = match feedback.kind {
                    CommandFeedbackKind::Notice => {
                        (cx.theme().primary, t!("agent-feedback-notice"))
                    }

                    CommandFeedbackKind::Status => {
                        (cx.theme().muted_foreground, t!("agent-feedback-status"))
                    }

                    CommandFeedbackKind::Error => (cx.theme().danger, t!("agent-feedback-error")),

                    CommandFeedbackKind::Queued => {
                        (cx.theme().warning, t!("agent-feedback-queued"))
                    }
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

        let session_state = self.session_state.render(
            &self.session.borrow().goal,
            self.session.borrow().plan_mode,
            cx,
        );

        let approval = self.render_approval_panel(cx);
        let questions = self.render_question_panel(window, cx);

        let action: ComposerAction = self.session.borrow().runtime.status().into();
        let running = action == ComposerAction::Stop;
        let update_suspended = self.session.borrow().runtime.update_suspension().is_some();
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
            cx.global::<AgentSettings>().terminal_background
        } else {
            cx.theme().sidebar
        };

        // Blank tabs expose recent sessions automatically; `/resume` can
        // request the same list after a conversation has started. A count
        // result reserves placeholder rows until the full entries arrive.
        let history_rows = self
            .history_ui
            .data
            .pending
            .unwrap_or(self.history_ui.data.sessions.len());

        let transcript_empty = self.transcript.read(cx).is_empty();
        let composer_empty = self.input.read(cx).text().len() == 0;

        let history = self
            .history_ui
            .mode
            .is_visible(transcript_empty, composer_empty, history_rows)
            .then(|| self.render_history(background, cx));

        // A list opened over a live conversation is a picker, and the
        // transcript behind it is not what the next click should reach. Blur
        // pushes it back a layer while keeping the tab recognizable as that
        // conversation; a blank tab has nothing to push back.
        let blur_transcript = history.is_some() && !transcript_empty;
        let now = Instant::now();

        let transcript_frost =
            self.history_ui
                .transcript_blur
                .drive(blur_transcript, now, window, cx);

        // One layer holds the pane for both the update and the start; a start
        // over an update is the more recent thing to say.
        let blocking_body = start_overlay.or(update_overlay);

        let blocking_frost = self
            .overlay_fade
            .drive(blocking_body.is_some(), now, window, cx);

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
                } else if this.session.borrow().input.approval().is_some() {
                    this.respond_approval("cancel", cx);
                } else if this.prompts.questions_open(&this.session.borrow().input) {
                    this.prompts.collapsed = true;

                    cx.notify();
                } else if this.session.borrow().runtime.status() == Status::Running {
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
                    // The layer swallows clicks aimed at the transcript; the
                    // list's outside-click handler still sees them and
                    // dismisses itself.
                    .when(!transcript_frost.gone(), |this| {
                        this.child(FrostedLayer::new(transcript_frost).light())
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
                transcript_column(
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
                                .children(self.attachments.render(cx))
                                .child(
                                    h_flex()
                                        .w_full()
                                        .px(px(COMPOSER_EDGE_INSET))
                                        .pt_3()
                                        .pb_1()
                                        // GPUI resolves these keystrokes
                                        // into Textarea actions before raw
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
                                                    cx.global::<AgentSettings>().newline_shortcut,
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

                                                    ComposerEnterBehavior::ActivateOrSubmit => this
                                                        .handle_palette_control(
                                                            PaletteControl::Activate,
                                                            window,
                                                            cx,
                                                        ),
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
                                        .text_size(px(cx.global::<AgentSettings>().font_size + 2.0))
                                        .child(div().flex_1().min_w_0().child(
                                            Textarea::new(&self.input).appearance(false).disabled(
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
                                        .child(div().flex_1().min_w_0().child(
                                            self.controls.render_row(
                                                &self.session.borrow().controls,
                                                self.kind,
                                                cx,
                                            ),
                                        ))
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
                                                .tooltip(t!("agent-action-stop-response"))
                                                .accessibility_label(t!(
                                                    "agent-action-stop-response",
                                                ))
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
                                                .tooltip(t!("agent-action-send-message"))
                                                .accessibility_label(t!(
                                                    "agent-action-send-message",
                                                ))
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
                    cx,
                )
                .pb_3()
                .pt_1()
            })
            // Painted last so it sits over the transcript and the composer.
            // Once the state it showed has ended the layer keeps fading with
            // nothing on it; the body belonged to that state.
            .when(!blocking_frost.gone(), |this| {
                this.child(
                    FrostedLayer::new(blocking_frost)
                        .padded()
                        .children(blocking_body),
                )
            })
    }
}

impl AgentPane {
    fn show_selected_text_menu(
        pane: WeakEntity<Self>,
        released_at: Point<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let selected_text = TextSelection::selected_text(window, cx).trim().to_string();

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
                menu.item(t!("agent-transcript-copy"), move |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                })
                .icon(IconName::Copy)
                .item(t!("agent-transcript-quote"), move |window, cx| {
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

/// Edge of the mark. Set to the size of a settings pill's own glyph, so the
/// row it stands in keeps one glyph size across its whole width.
const LAST_RESPONSE_MARK: f32 = 12.0;

/// How far into the window a conversation has to have drifted before the
/// composer says so, and before it says so in the danger colour. The window is
/// the one a provider's prompt cache is expected to hold, so the first mark
/// says the next message is going to start costing more than the last one did,
/// and the second says it is about to cost a full re-read of the context.
const LAST_RESPONSE_WARNING: f32 = 0.5;

const LAST_RESPONSE_DANGER: f32 = 0.9;

/// How loudly the composer marks a conversation that has been sitting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LastResponseTone {
    Warning,
    Danger,
}

/// The mark a settled conversation carries, from how long it has been sitting.
///
/// Under half the window there is nothing worth saying: a conversation picked
/// up that soon costs what it would have cost immediately, and a reading that
/// is always on screen is one the eye stops seeing. Past the window the answer
/// stops changing, which is the same answer as the last reading inside it.
fn last_response_tone(seconds: u64) -> Option<LastResponseTone> {
    let drift = seconds as f32 / LAST_RESPONSE_LIMIT.as_secs() as f32;

    if drift >= LAST_RESPONSE_DANGER {
        Some(LastResponseTone::Danger)
    } else if drift >= LAST_RESPONSE_WARNING {
        Some(LastResponseTone::Warning)
    } else {
        None
    }
}

impl AgentPane {
    /// How long ago the agent last answered, beside the composer's controls.
    ///
    /// Drawn as a mark rather than as a reading: the number itself only
    /// matters once it is large enough to change what the next message costs,
    /// and until then a line of text beside the settings is one more thing to
    /// read past on the way to sending. The wording it used to carry is on the
    /// mark's tooltip, and in its accessible label.
    ///
    /// Absent until a turn has settled, and while one is running: the
    /// transcript's own live "Working for" reading is the answer then, and two
    /// clocks a few pixels apart would be read as disagreeing.
    fn render_last_response(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let at = self
            .session
            .borrow()
            .conversation
            .borrow()
            .last_response_at?;

        if self.transcript.read(cx).is_working() {
            return None;
        }

        let seconds = at.elapsed().as_secs();

        let color = match last_response_tone(seconds)? {
            LastResponseTone::Warning => cx.theme().warning,
            LastResponseTone::Danger => cx.theme().danger,
        };

        let label = last_response_label(seconds);
        let tooltip = label.clone();

        Some(
            div()
                .id("agent-last-response")
                .flex_none()
                .flex()
                .items_center()
                .aria_label(label)
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .child(
                    Icon::new(IconName::TriangleAlert)
                        .size(px(LAST_RESPONSE_MARK))
                        .text_color(color),
                )
                .into_any_element(),
        )
    }
}
