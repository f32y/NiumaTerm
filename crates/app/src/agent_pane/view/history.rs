use crate::agent_pane::transcript::relative_time;
use crate::agent_pane::*;

pub(in crate::agent_pane::view) fn queued_message_label(
    messages: &VecDeque<String>,
) -> Option<String> {
    (!messages.is_empty()).then(|| {
        let text = messages
            .iter()
            .map(|message| message.lines().collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join(" · ");

        format!("Queued message: {text}")
    })
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
    pub(in crate::agent_pane::view) fn render_history(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let rows = self
            .history_ui
            .pending
            .unwrap_or(self.history_ui.sessions.len());
        let body_height =
            px((Self::HISTORY_ROW_HEIGHT * rows as f32).min(Self::HISTORY_MAX_HEIGHT));

        let body: AnyElement = if self.history_ui.pending.is_some() {
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
                            if visible_range.end >= this.history_ui.sessions.len()
                                && let Some(Backend::Codex(session)) = this.session.as_mut()
                            {
                                session.request_more_history();
                            }

                            visible_range
                                .map(|index| this.render_history_row(index, cx))
                                .collect()
                        },
                    )
                    .track_scroll(&self.history_ui.scroll)
                    .with_sizing_behavior(ListSizingBehavior::Infer),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(px(16.))
                        .child(Scrollbar::vertical(&self.history_ui.scroll)),
                )
                .into_any_element()
        };

        // Centered at 90% of the composer width, on the pane background
        // behind the shell: an outlined strip on a deeper tint, rounded only
        // at the top. The shell overlaps its lower edge (negative margin on
        // the shell), so the strip reads as a layer sliding out from behind
        // the front card. The extra bottom padding is clearance for that
        // overlap — without it the card would cover the last row.
        div()
            .w_full()
            .flex()
            .justify_center()
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.history_ui.mode = RecentSessionsMode::Hidden;
                cx.notify();
            }))
            .child(
                v_flex()
                    .w(relative(0.95))
                    .rounded_t(UI_RADIUS)
                    .border_1()
                    .border_b_0()
                    .border_color(cx.theme().border.opacity(0.6))
                    .bg(cx.theme().muted.alpha(1.0))
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
        let Some(session) = self.history_ui.sessions.get(index) else {
            return div().into_any_element();
        };
        let hover_bg = cx.theme().muted.opacity(0.4);
        let selected = self.history_ui.selected == index
            && matches!(
                self.history_ui.mode,
                RecentSessionsMode::Automatic | RecentSessionsMode::Open
            )
            && self.input.read(cx).text().len() == 0;

        h_flex()
            .id(("history-row", index))
            .h(px(Self::HISTORY_ROW_HEIGHT))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .rounded(UI_RADIUS)
            .cursor_pointer()
            .when(selected, |this| this.bg(cx.theme().muted.opacity(0.7)))
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
}
