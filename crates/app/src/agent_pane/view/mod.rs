mod attachments;
mod banners;
mod history;
mod last_response;
mod session_state;
mod settings_row;

#[cfg(test)]
mod tests;

use gpui_component::input::Paste;
use nmt_i18n::i18n;

use crate::agent_pane::composer::{
    CommandFeedbackKind, ComposerAction, PaletteControl, composer_action,
};
use crate::agent_pane::*;
use crate::terminal::frame::theme_default_background;
use crate::ui::main_view_background_opacity;

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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let command_palette = self.render_command_palette(cx);
        let command_feedback = self.visible_command_feedback().map(|feedback| {
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

        let running = composer_action(self.status) == ComposerAction::Stop;
        let update_suspended = self.update_suspension.is_some();
        let update_banner = self.render_update_banner(cx);
        let rewind_active = self.rewind.state.is_some();
        let rewind_processing = self
            .rewind
            .state
            .as_ref()
            .is_some_and(|state| !state.is_picker());
        let session_loading = self.history_ui.mode == RecentSessionsMode::Loading;
        let background = if cx
            .global::<AppSettings>()
            .agent_pane_use_terminal_background
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
        let history = self
            .history_ui
            .mode
            .is_visible(self.transcript.read(cx).is_empty(), history_rows)
            .then(|| self.render_history(cx));

        v_flex()
            .size_full()
            // The outer frame matches the window chrome. The Agent surface owns
            // its fill so an opaque main view does not color the rounded frame.
            .bg(background.alpha(main_view_background_opacity(cx)))
            .rounded(UI_RADIUS - px(1.))
            .overflow_hidden()
            .track_focus(&self.focus)
            // Escape force-stops the agent whenever the pane or composer has
            // focus. The input propagates Escape here when the editor did not
            // consume it (inline completion, IME), and transcript clicks focus
            // the pane below. A pending approval is cancelled (deny +
            // interrupt), while a running turn is interrupted directly.
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                if this
                    .rewind
                    .state
                    .as_ref()
                    .is_some_and(RewindState::is_picker)
                {
                    this.cancel_rewind_picker(cx);
                } else if this.pending_approval.is_some() {
                    this.respond_approval("cancel", cx);
                } else if this.pending_questions.is_some() {
                    // Dismissing questions is not cancelling the turn: the
                    // model is told to assume and continue, so Escape here
                    // deliberately does not interrupt.
                    this.respond_questions(false, cx);
                } else if this.status == Status::Running {
                    this.interrupt_from_ui(window, cx);
                }
            }))
            // The agent tab is a terminal surface stand-in, so it overrides the
            // chrome's UI font with its own configured font (Settings → Agent
            // Font), same as the terminal pane does with the terminal font.
            .font_family(cx.global::<AppSettings>().agent_font_family.clone())
            .text_size(px(cx.global::<AppSettings>().agent_font_size as f32))
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
                        cx.listener(|this, _, window, cx| {
                            this.focus(window, cx);
                        }),
                    )
                    .child(self.transcript.clone()),
            )
            .child({
                // Composer area: auxiliary strips sit outside the bordered,
                // shadowed shell on a deeper surface. History is absolutely
                // anchored above the shell because it only exists while the
                // transcript is empty; loading it must never participate in
                // composer height calculation. Both strips are painted before
                // the shell, whose edge and shadow keep them visibly tucked
                // behind the input card.
                div().w_full().px_3().pb_3().pt_1().child(
                    div()
                        .relative()
                        .pb(px(30.))
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
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .flex()
                                .justify_center()
                                .child(self.render_composer_status(cx)),
                        )
                        .child(
                            v_flex()
                                .w_full()
                                .rounded(UI_RADIUS)
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
                                    div()
                                        .px_3()
                                        .pt_3()
                                        .pb_2()
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
                                                    cx.global::<AppSettings>().newline_shortcut,
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
                                        .text_size(px(cx.global::<AppSettings>().agent_font_size
                                            as f32
                                            + 2.0))
                                        .child(Input::new(&self.input).appearance(false).disabled(
                                            rewind_processing
                                                || session_loading
                                                || update_suspended,
                                        )),
                                )
                                .child(
                                    h_flex()
                                        .w_full()
                                        .px_2p5()
                                        .pb_2p5()
                                        .pt_0p5()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .child(self.render_settings_row(cx)),
                                        )
                                        .children(self.render_last_response(cx))
                                        // Stop replaces Send in place while a
                                        // turn runs.
                                        .child(if running {
                                            Button::new("agent-send")
                                                .primary()
                                                .size(px(32.))
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
                                                    rewind_active
                                                        || session_loading
                                                        || update_suspended,
                                                )
                                                .size(px(32.))
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
    }
}

impl AgentPane {
    // A pending approval transforms the composer: the panel slots into
    // the shell's top (the shell's rounded frame clips it), and the
    // decision buttons escalate left to right.
}
