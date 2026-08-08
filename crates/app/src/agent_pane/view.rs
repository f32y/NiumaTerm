use crate::agent_pane::composer::{
    CommandFeedbackKind, ComposerAction, PaletteControl, composer_action,
};
use crate::agent_pane::transcript::{compact_token_count, permission_icon, relative_time};
use crate::agent_pane::*;
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
        let collapse = cx.global::<AppSettings>().collapse_tool_calls;
        let command_palette = self.render_command_palette(cx);
        let command_feedback = self.command_feedback.as_ref().map(|feedback| {
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

        // A pending approval transforms the composer: the panel slots into
        // the shell's top (the shell's rounded frame clips it), and the
        // decision buttons escalate left to right.
        let approval = self.pending_approval.as_ref().map(|approval| {
            v_flex()
                .w_full()
                .px_4()
                .py_3()
                .gap_2()
                .border_b_1()
                .border_color(cx.theme().border.opacity(0.65))
                .bg(cx.theme().muted.opacity(0.2))
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().muted_foreground)
                        .child("PENDING APPROVAL"),
                )
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded(UI_RADIUS)
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().background.opacity(0.7))
                        .text_sm()
                        .child(approval.clone()),
                )
                .child(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("approval-cancel")
                                .ghost()
                                .label("Cancel turn")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.respond_approval("cancel", cx)
                                })),
                        )
                        .child(
                            Button::new("approval-decline")
                                .outline()
                                .label("Decline")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.respond_approval("decline", cx)
                                })),
                        )
                        .child(
                            Button::new("approval-session")
                                .outline()
                                .label("Always allow this session")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.respond_approval("acceptForSession", cx)
                                })),
                        )
                        .child(
                            Button::new("approval-accept")
                                .primary()
                                .label("Approve once")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.respond_approval("accept", cx)
                                })),
                        ),
                )
        });

        let running = composer_action(self.status) == ComposerAction::Stop;
        let update_suspended = self.update_suspension.is_some();
        let update_banner = self.update_suspension.as_ref().map(|state| {
            let (label, detail, failed) = match state {
                UpdateSuspension::Waiting => (
                    "WAITING FOR UPDATE",
                    "New provider input is paused until this tab becomes recoverably idle.",
                    false,
                ),
                UpdateSuspension::Stopping => (
                    "STOPPING AGENT",
                    "The provider process is closing while this tab stays open.",
                    false,
                ),
                UpdateSuspension::Updating => (
                    "UPDATING PROVIDER",
                    "The provider binary is being updated; transcript and draft are retained.",
                    false,
                ),
                UpdateSuspension::Reconnecting => (
                    "RECONNECTING",
                    "Restoring this tab to its provider conversation.",
                    false,
                ),
                UpdateSuspension::Failed(message) => ("RECONNECT FAILED", message.as_str(), true),
            };

            h_flex()
                .w_full()
                .px_4()
                .py_2()
                .gap_3()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(if failed {
                    cx.theme().danger.opacity(0.12)
                } else {
                    cx.theme().primary.opacity(0.10)
                })
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if failed {
                            cx.theme().danger
                        } else {
                            cx.theme().primary
                        })
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(detail.to_string()),
                )
                .when(failed, |row| {
                    row.child(
                        Button::new("agent-update-retry")
                            .outline()
                            .small()
                            .label("Retry")
                            .on_click(cx.listener(|this, _, _, cx| this.retry_update_recovery(cx))),
                    )
                    .child(
                        Button::new("agent-update-new-session")
                            .danger()
                            .small()
                            .label("Start new session")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.start_new_after_update_failure(cx)
                            })),
                    )
                })
        });
        let rewind_active = self.rewind_state.is_some();
        let rewind_processing = self
            .rewind_state
            .as_ref()
            .is_some_and(|state| !state.is_picker());
        let transcript_has_hidden_content_below = self.transcript_has_hidden_content_below();
        let transcript_scrolled_from_top = self.transcript_has_hidden_content_above();

        // The history list only makes sense while the tab is a blank slate:
        // no transcript yet and no conversation committed to. It shows as
        // placeholders once the count pass promises rows, then as real rows.
        let history_rows = self.history_pending.unwrap_or(self.history.len());
        let history = (self.items.is_empty() && history_rows > 0 && !self.history_dismissed)
            .then(|| self.render_history(cx));

        v_flex()
            .size_full()
            // The outer frame matches the window chrome. The Agent surface owns
            // its fill so an opaque main view does not color the rounded frame.
            .bg(cx.theme().sidebar.alpha(main_view_background_opacity(cx)))
            .rounded(UI_RADIUS - px(1.))
            .overflow_hidden()
            .track_focus(&self.focus)
            // Escape force-stops the agent whenever the pane or composer has
            // focus. The input propagates Escape here when the editor did not
            // consume it (inline completion, IME), and transcript clicks focus
            // the pane below. A pending approval is cancelled (deny +
            // interrupt), while a running turn is interrupted directly.
            .on_action(cx.listener(|this, _: &Escape, _, cx| {
                if this
                    .rewind_state
                    .as_ref()
                    .is_some_and(RewindState::is_picker)
                {
                    this.cancel_rewind_picker(cx);
                } else if this.pending_approval.is_some() {
                    this.respond_approval("cancel", cx);
                } else if this.status == Status::Running {
                    this.interrupt(cx);
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
                            // Reading or selecting transcript content should
                            // leave Escape routed to the pane-level interrupt
                            // handler instead of whichever control previously
                            // held keyboard focus.
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    window.focus(&this.focus, cx);
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
                                        .child(
                                            Input::new(&self.input)
                                                .appearance(false)
                                                .disabled(rewind_processing || update_suspended),
                                        ),
                                )
                                .children(command_feedback)
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
                                                .on_click(
                                                    cx.listener(|this, _, _, cx| {
                                                        this.interrupt(cx)
                                                    }),
                                                )
                                        } else {
                                            Button::new("agent-send")
                                                .primary()
                                                .disabled(rewind_active || update_suspended)
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
    /// Height of one history row; all rows are uniform, which is what lets
    /// the virtual list precompute its scroll geometry.
    const HISTORY_ROW_HEIGHT: f32 = 28.0;
    /// Ten rows visible by default; more scroll within the fixed viewport.
    const HISTORY_MAX_HEIGHT: f32 = Self::HISTORY_ROW_HEIGHT * 10.0;

    /// The resumable-sessions block slotted into the composer shell above the
    /// input: a strip at 90% of the composer width on a slightly deeper
    /// surface, reading as a layer tucked behind the input card (t3code's
    /// context-strip look). While only the count pass has finished it shows
    /// skeleton rows at the final height, so the composer doesn't jump when
    /// the real rows land; rows render through a virtual list, so hundreds
    /// of persisted sessions cost only the visible ten.
    fn render_history(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let rows = self.history_pending.unwrap_or(self.history.len());
        let body_height =
            px((Self::HISTORY_ROW_HEIGHT * rows as f32).min(Self::HISTORY_MAX_HEIGHT));

        let body: AnyElement = if self.history_pending.is_some() {
            // Both loading and loaded bodies use the same explicit viewport
            // height. The virtual list's inferred first-frame measurement
            // must not move the composer when it replaces these placeholders.
            v_flex()
                .w_full()
                .h(body_height)
                .flex_none()
                .px_2()
                .gap_0()
                .children((0..rows.min(10)).map(|i| {
                    h_flex()
                        .h(px(Self::HISTORY_ROW_HEIGHT))
                        .w_full()
                        .px_2()
                        .items_center()
                        .child(
                            Skeleton::new()
                                .h(px(14.))
                                .w(relative(if i % 2 == 0 { 0.72 } else { 0.55 }))
                                .rounded(UI_RADIUS),
                        )
                }))
                .into_any_element()
        } else {
            let row_sizes = Rc::new(vec![size(px(0.), px(Self::HISTORY_ROW_HEIGHT)); rows]);

            div()
                .relative()
                .w_full()
                .h(body_height)
                .flex_none()
                .overflow_hidden()
                .px_2()
                .child(
                    v_virtual_list(
                        cx.entity(),
                        "agent-history",
                        row_sizes,
                        move |this, visible_range, _, cx| {
                            // The final page in view is the cue to fetch
                            // the next one (no-op without a cursor, and
                            // only Codex pages from the backend).
                            if visible_range.end >= this.history.len()
                                && let Some(Backend::Codex(session)) = this.session.as_mut()
                            {
                                session.request_more_history();
                            }

                            visible_range
                                .map(|index| this.render_history_row(index, cx))
                                .collect()
                        },
                    )
                    .track_scroll(&self.history_scroll)
                    .with_sizing_behavior(ListSizingBehavior::Infer),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(px(16.))
                        .child(Scrollbar::vertical(&self.history_scroll)),
                )
                .into_any_element()
        };

        // Centered at 90% of the composer width, on the pane background
        // behind the shell: an outlined strip on a deeper tint, rounded only
        // at the top. The shell overlaps its lower edge (negative margin on
        // the shell), so the strip reads as a layer sliding out from behind
        // the front card. The extra bottom padding is clearance for that
        // overlap — without it the card would cover the last row.
        div().w_full().flex().justify_center().child(
            v_flex()
                .w(relative(0.95))
                .rounded_t(UI_RADIUS)
                .border_1()
                .border_b_0()
                .border_color(cx.theme().border.opacity(0.6))
                .bg(cx.theme().muted.opacity(0.55))
                .pb(px(20.))
                .child(
                    div()
                        .px_4()
                        .pt_2()
                        .pb_1()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().muted_foreground)
                        .child("RECENT SESSIONS"),
                )
                .child(body),
        )
    }

    /// One history row: title, branch, and relative time, in the settings
    /// row's ghost-control idiom (small, muted, hover lifts the foreground).
    fn render_history_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(session) = self.history.get(index) else {
            return div().into_any_element();
        };
        let hover_bg = cx.theme().muted.opacity(0.4);

        h_flex()
            .id(("history-row", index))
            .h(px(Self::HISTORY_ROW_HEIGHT))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .rounded(UI_RADIUS)
            .cursor_pointer()
            .hover(move |style| style.bg(hover_bg))
            .on_click(cx.listener(move |this, _, _, cx| this.resume_session(index, cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().foreground.opacity(0.82))
                    .child(session.title.clone()),
            )
            .children(session.branch.clone().map(|branch| {
                h_flex()
                    .flex_none()
                    .gap_1()
                    .items_center()
                    .max_w(px(180.))
                    .child(
                        Icon::new(IconName::GitBranch)
                            .size_3()
                            .text_color(cx.theme().muted_foreground.opacity(0.7)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground.opacity(0.7))
                            .child(branch),
                    )
            }))
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground.opacity(0.55))
                    .child(relative_time(session.last_active)),
            )
            .into_any_element()
    }

    fn render_composer_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let branch = self.git_branch.clone().unwrap_or_else(|| {
            if self.git_branch_ready {
                "No Git branch".to_string()
            } else {
                "Detecting branch…".to_string()
            }
        });
        let branch_opacity = if self.git_branch.is_some() {
            0.72
        } else {
            0.48
        };

        let usage = self.context_window_usage.map(|usage| {
            let (label, color) = match usage.max_tokens {
                Some(max_tokens) if max_tokens > 0 => {
                    let remaining_tokens = max_tokens.saturating_sub(usage.used_tokens);
                    let remaining_percent =
                        (remaining_tokens as f64 * 100.0 / max_tokens as f64).round() as u64;
                    let color = if remaining_percent <= 10 {
                        cx.theme().danger
                    } else {
                        cx.theme().muted_foreground.opacity(0.72)
                    };

                    (
                        format!(
                            "{} used · {remaining_percent}% left",
                            compact_token_count(usage.used_tokens)
                        ),
                        color,
                    )
                }
                _ => (
                    format!("{} used", compact_token_count(usage.used_tokens)),
                    cx.theme().muted_foreground.opacity(0.72),
                ),
            };

            h_flex()
                .flex_none()
                .gap_1p5()
                .items_center()
                .text_color(color)
                .child(Icon::new(IconName::ChartPie).size_3())
                .child(div().child(label))
        });

        v_flex()
            .w(relative(0.95))
            .rounded_b(UI_RADIUS)
            .border_1()
            .border_t_0()
            .border_color(cx.theme().border.opacity(0.6))
            .bg(cx.theme().popover)
            .pt(px(18.))
            .child(
                h_flex()
                    .w_full()
                    .min_h(px(26.))
                    .px_4()
                    .pt(px(6.))
                    .pb(px(6.))
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .text_xs()
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_1p5()
                            .items_center()
                            .text_color(cx.theme().muted_foreground.opacity(branch_opacity))
                            .child(Icon::new(IconName::GitBranch).size_3())
                            .child(div().min_w_0().truncate().child(branch)),
                    )
                    .children(usage),
            )
            .into_any_element()
    }

    /// The dropdown row under the input, per agent kind.
    fn render_settings_row(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.kind {
            AgentKind::Codex => self.render_codex_settings_row(cx).into_any_element(),
            AgentKind::Claude => self.render_claude_settings_row(cx).into_any_element(),
        }
    }

    /// Claude settings: model, permission mode, and reasoning effort. The
    /// model catalog comes from the initialize handshake; model and
    /// permission changes apply via control requests before the next message.
    /// Effort has no control request — it's applied by sending the `/effort`
    /// slash command as a user message, which the CLI handles locally
    /// (instant, no model call), so the picker sends it immediately as its
    /// own mini-turn. The effort levels are per model; models without effort
    /// support (e.g. Haiku) get no picker.
    fn render_claude_settings_row(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let model_options: Vec<(String, String)> = self
            .models
            .iter()
            .map(|m| (m.model.clone(), m.display.clone()))
            .collect();
        let permission_options: Vec<(String, String)> = stream_json::PERMISSION_OPTIONS
            .iter()
            .map(|v| (v.to_string(), v.to_string()))
            .collect();
        let effort_options: Vec<(String, String)> = self
            .models
            .iter()
            .find(|m| Some(&m.model) == self.settings.model.as_ref())
            .map(|m| m.efforts.iter().map(|v| (v.clone(), v.clone())).collect())
            .unwrap_or_default();

        let model = Self::setting_picker(
            cx,
            "agent-model",
            "model",
            IconName::Cpu,
            self.settings.model.clone(),
            model_options,
            true,
            |this, value, cx| {
                this.settings.model = Some(value);
                this.remember_thread_defaults(cx);
            },
        )
        .into_any_element();
        let permission = Self::setting_picker(
            cx,
            "agent-permission",
            "permissions",
            permission_icon(self.settings.approval.as_deref()),
            self.settings.approval.clone(),
            permission_options,
            false,
            |this, value, cx| {
                this.settings.approval = Some(value);
                this.remember_thread_defaults(cx);
            },
        )
        .into_any_element();

        let mut row = h_flex()
            .w_full()
            .gap_1()
            .flex_wrap()
            .text_color(cx.theme().muted_foreground)
            .child(Self::settings_group("Model", vec![model], cx))
            .child(Self::settings_group(
                "Execution policy",
                vec![permission],
                cx,
            ));

        if !effort_options.is_empty() {
            let effort = Self::setting_picker(
                cx,
                "agent-effort",
                "effort",
                IconName::Gauge,
                // The protocol never reports the session's current effort;
                // until the user picks one, the honest label is the CLI's
                // own per-model default rather than an empty dash.
                self.settings
                    .effort
                    .clone()
                    .or_else(|| Some("default".to_string())),
                effort_options,
                false,
                |this, value, cx| {
                    this.settings.effort = Some(value.clone());
                    this.remember_thread_defaults(cx);
                    this.send_text(format!("/effort {value}"), cx);
                },
            )
            .into_any_element();
            row = row.child(Self::settings_group("Quality and cost", vec![effort], cx));
        }

        row
    }

    /// Codex settings: model, approval policy, sandbox, reasoning effort, and
    /// service tier. Values are thread settings sent as overrides on the next
    /// `turn/start`.
    fn render_codex_settings_row(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let model_options: Vec<(String, String)> = self
            .models
            .iter()
            .map(|m| (m.model.clone(), m.display.clone()))
            .collect();
        // Service tiers are per model, and the catalog only lists the
        // additional tiers (e.g. "Fast") — the normal tier is implicit, so
        // the menu carries a synthetic entry for it. Empty protocol value =
        // normal = explicit `serviceTier: null` on the next turn.
        let mut tier_options: Vec<(String, String)> = vec![(String::new(), "normal".to_string())];

        tier_options.extend(
            self.models
                .iter()
                .find(|m| Some(&m.model) == self.settings.model.as_ref())
                .map(|m| m.tiers.clone())
                .unwrap_or_default(),
        );
        let approval_options: Vec<(String, String)> = app_server::APPROVAL_OPTIONS
            .iter()
            .map(|v| (v.to_string(), v.to_string()))
            .collect();
        let sandbox_options: Vec<(String, String)> = app_server::SANDBOX_OPTIONS
            .iter()
            .map(|(v, label)| (v.to_string(), label.to_string()))
            .collect();
        let effort_options: Vec<(String, String)> = app_server::EFFORT_OPTIONS
            .iter()
            .map(|v| (v.to_string(), v.to_string()))
            .collect();

        let model = Self::setting_picker(
            cx,
            "agent-model",
            "model",
            IconName::Cpu,
            self.settings.model.clone(),
            model_options,
            true,
            |this, value, cx| {
                // A tier the new model doesn't offer falls back to that
                // model's default tier instead of erroring the next turn.
                if let Some(info) = this.models.iter().find(|m| m.model == value)
                    && !this
                        .settings
                        .tier
                        .as_ref()
                        .is_some_and(|tier| info.tiers.iter().any(|(id, _)| id == tier))
                {
                    this.settings.tier = info.default_tier.clone();
                }
                this.settings.model = Some(value);
                this.remember_thread_defaults(cx);
            },
        )
        .into_any_element();
        let approval = Self::setting_picker(
            cx,
            "agent-approval",
            "approval",
            permission_icon(self.settings.approval.as_deref()),
            self.settings.approval.clone(),
            approval_options,
            false,
            |this, value, cx| {
                this.settings.approval = Some(value);
                this.remember_thread_defaults(cx);
            },
        )
        .into_any_element();
        let sandbox = Self::setting_picker(
            cx,
            "agent-sandbox",
            "sandbox",
            IconName::Shield,
            self.settings.sandbox.clone(),
            sandbox_options,
            false,
            |this, value, cx| {
                this.settings.sandbox = Some(value);
                this.remember_thread_defaults(cx);
            },
        )
        .into_any_element();
        let effort = Self::setting_picker(
            cx,
            "agent-effort",
            "effort",
            IconName::Gauge,
            self.settings.effort.clone(),
            effort_options,
            false,
            |this, value, cx| {
                this.settings.effort = Some(value);
                this.remember_thread_defaults(cx);
            },
        )
        .into_any_element();
        let tier = Self::setting_picker(
            cx,
            "agent-tier",
            "tier",
            IconName::Zap,
            Some(self.settings.tier.clone().unwrap_or_default()),
            tier_options,
            false,
            |this, value, cx| {
                this.settings.tier = (!value.is_empty()).then_some(value);
                this.remember_thread_defaults(cx);
            },
        )
        .into_any_element();

        h_flex()
            .w_full()
            .gap_1()
            .flex_wrap()
            .text_color(cx.theme().muted_foreground)
            .child(Self::settings_group("Model", vec![model], cx))
            .child(Self::settings_group(
                "Execution policy",
                vec![approval, sandbox],
                cx,
            ))
            .child(Self::settings_group(
                "Quality and cost",
                vec![effort, tier],
                cx,
            ))
    }

    fn settings_group(
        label: &'static str,
        controls: Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        h_flex()
            .id(label)
            .aria_label(label)
            .gap_0p5()
            .p(px(1.))
            .rounded(UI_RADIUS)
            .border_1()
            .border_color(cx.theme().border.opacity(0.65))
            .bg(cx.theme().muted.opacity(0.2))
            .children(controls)
    }

    /// One dropdown showing `icon · current value · chevron`. Every picker uses
    /// the same quiet color treatment; the model remains wider so its value is
    /// easier to scan. Menus keep the existing protocol values and setters.
    fn setting_picker(
        cx: &mut Context<Self>,
        id: &'static str,
        name: &'static str,
        icon: IconName,
        current: Option<String>,
        options: Vec<(String, String)>,
        is_model: bool,
        set: fn(&mut Self, String, &mut Context<Self>),
    ) -> impl IntoElement + use<> {
        let pane = cx.entity();

        // Show the display label of the current protocol value when we know it.
        let current_label = current
            .as_ref()
            .map(|value| {
                options
                    .iter()
                    .find(|(option_value, _)| option_value == value)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_else(|| value.clone())
            })
            .unwrap_or_else(|| "—".to_string());

        Button::new(id)
            .ghost()
            .when(is_model, |this| this.min_w(px(120.)))
            .small()
            .tooltip(name)
            .aria_label(format!("{name}: {current_label}"))
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Icon::new(icon)
                            .size_4()
                            .text_color(cx.theme().muted_foreground.opacity(0.8)),
                    )
                    .child(div().text_sm().child(current_label))
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size_3()
                            .text_color(cx.theme().muted_foreground.opacity(0.7)),
                    ),
            )
            // Anchored bottom-left so the menu opens upward — the row sits at
            // the bottom edge of the pane.
            .dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, _, _| {
                let mut menu = menu;

                if options.is_empty() {
                    menu = menu.label("loading…");
                }

                for (value, label) in options.clone() {
                    let pane = pane.clone();
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        pane.update(cx, |this, cx| {
                            set(this, value.clone(), cx);
                            cx.notify();
                        });
                    }));
                }

                menu
            })
    }
}
