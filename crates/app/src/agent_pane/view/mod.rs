mod banners;
mod history;
mod settings_row;

#[cfg(test)]
mod tests;

use crate::agent_pane::composer::{
    CommandFeedbackKind, ComposerAction, PaletteControl, composer_action,
};
use crate::agent_pane::view::history::queued_message_label;
use crate::agent_pane::*;
use crate::terminal::frame::theme_default_background;
use crate::ui::main_view_background_opacity;

struct StopResponseIcon;

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
        let settings = cx.global::<AppSettings>();
        let collapse = settings.collapse_tool_calls;
        self.transcript_list
            .set_smooth_wheel_enabled(settings.smooth_scrolling.agent_enabled());
        let command_palette = self.render_command_palette(cx);
        let command_feedback = self.palette.feedback.as_ref().map(|feedback| {
            let (color, label) = match feedback.kind {
                CommandFeedbackKind::Notice => (cx.theme().primary, "NOTICE"),
                CommandFeedbackKind::Error => (cx.theme().danger, "ERROR"),
                CommandFeedbackKind::Queued => (cx.theme().warning, "QUEUED"),
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
        let queued_message = queued_message_label(&self.queued_user_messages).map(|label| {
            h_flex()
                .w_full()
                .px_3()
                .py_1p5()
                .border_b_1()
                .border_color(cx.theme().border.opacity(0.6))
                .bg(cx.theme().muted.opacity(0.3))
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(div().min_w_0().truncate().child(label))
        });

        // Transcript rows, one folded/expanded section per turn (entries are
        // tagged with a monotonic turn id, so turns are contiguous slices).
        // Only the visible slice becomes elements; the spec diff tells the
        // list which rows changed shape.
        let specs = self.build_row_specs(collapse);
        self.sync_transcript_list(specs);

        let font = (
            cx.global::<AppSettings>().agent_font_family.clone(),
            cx.global::<AppSettings>().agent_font_size,
        );
        if self.transcript_font != font {
            self.transcript_font = font;
            self.transcript_list.remeasure();
        }

        let approval = self.render_approval_panel(cx);

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
        let transcript_has_hidden_content_below = self.transcript_has_hidden_content_below();
        let transcript_scrolled_from_top = self.transcript_has_hidden_content_above();
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
            .is_visible(self.items.is_empty(), history_rows)
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
                // The scrollbar must sit OUTSIDE the scrolling element (a
                // child would scroll away with the content), so a relative
                // wrapper hosts the scroll area and the overlay bar.
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("agent-transcript")
                            .size_full()
                            .on_prepaint({
                                let pane = cx.entity().downgrade();
                                move |bounds, _, cx| {
                                    pane.update(cx, |this, cx| {
                                        let width = bounds.size.width;
                                        if this.transcript_width != Some(width) {
                                            this.transcript_width = Some(width);
                                            this.transcript_list.remeasure();
                                            cx.notify();
                                        }
                                    })
                                    .ok();
                                }
                            })
                            // Selectable transcript text claims focus during
                            // mouse-down dispatch, so restore the composer on
                            // release. Escape then reaches the pane-level
                            // interrupt handler through the input.
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    this.focus(window, cx);
                                }),
                            )
                            .child(
                                // Element callbacks run after this render's
                                // entity lease is released, so the row builder
                                // re-enters the pane through a weak handle.
                                list(self.transcript_list.clone(), {
                                    let this = cx.entity().downgrade();
                                    move |ix, window, cx| {
                                        this.update(cx, |this, cx| this.render_row(ix, window, cx))
                                            .unwrap_or_else(|_| div().into_any_element())
                                    }
                                })
                                .size_full()
                                .pt(px(16.)),
                            ),
                    )
                    .children(transcript_scrolled_from_top.then(|| {
                        // This decorative overlay has no handlers, so text
                        // selection and wheel input continue to reach the list.
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right(px(16.))
                            .h(px(24.))
                            .bg(linear_gradient(
                                180.,
                                linear_color_stop(cx.theme().sidebar, 0.),
                                linear_color_stop(cx.theme().sidebar.opacity(0.), 1.),
                            ))
                    }))
                    // The bare Scrollbar element carries no inset of its own,
                    // so it lands at its static flow position (below the
                    // full-height sibling); the pinned strip gives it a
                    // deterministic containing block at the right edge.
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(16.))
                            .child(Scrollbar::vertical(&self.transcript_list)),
                    )
                    .when(transcript_has_hidden_content_below, |this| {
                        this.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom(px(12.))
                                .flex()
                                .justify_center()
                                .child(
                                    Button::new("agent-jump-to-bottom")
                                        .outline()
                                        .small()
                                        .rounded_full()
                                        .icon(IconName::ArrowDown)
                                        .tooltip("Jump to latest")
                                        .shadow_md()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.scroll_transcript_to_bottom();
                                            cx.notify();
                                        })),
                                ),
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
                                .children(command_feedback)
                                .children(queued_message)
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
                                                if action.shift || action.secondary {
                                                    cx.propagate();
                                                } else {
                                                    this.handle_palette_control(
                                                        PaletteControl::Activate,
                                                        window,
                                                        cx,
                                                    );
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
                                        // Stop replaces Send in place while a
                                        // turn runs.
                                        .child(if running {
                                            Button::new("agent-send")
                                                .danger()
                                                .size(px(32.))
                                                .rounded_full()
                                                .icon(StopResponseIcon)
                                                .tooltip("Stop response")
                                                .aria_label("Stop response")
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
                                                .tooltip("Send message")
                                                .aria_label("Send message")
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
