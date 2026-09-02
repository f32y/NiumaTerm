use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, FontWeight, Hsla, IntoElement, ListSizingBehavior, MouseMoveEvent, Pixels,
    Point, div, px, relative, size,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::scroll::Scrollbar;
use gpui_component::skeleton::Skeleton;
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex, v_virtual_list,
};
use nmt_agent_utils::chat::{QueuedPrompt, SessionScope, SessionSummary};
use nmt_i18n::i18n;

use crate::composer::visible_prompt;
use crate::session::{directories_match, directory_label};
use crate::settings::UI_RADIUS;
use crate::transcript::relative_time;
use crate::{AgentPane, RecentSessionsMode, SessionHistoryUi};

/// One queued prompt on one line. A prompt spanning several lines is folded
/// into one so every waiting row costs the composer the same height.
pub(super) fn queued_message_label(prompt: &QueuedPrompt) -> String {
    let text = visible_prompt(&prompt.text)
        .lines()
        .collect::<Vec<_>>()
        .join(" ");

    i18n("agent-history-queued-message").replace("{text}", &text)
}

impl SessionHistoryUi {
    /// Hand the highlight to the row under the pointer, reporting whether the
    /// highlight moved.
    ///
    /// Guarded on the pointer having actually moved. Keyboard navigation
    /// scrolls the list to keep its row in view, which slides a different row
    /// under a pointer resting over the strip; letting that count as pointing
    /// would take the highlight straight back off the arrow keys. Real
    /// movement takes it back unconditionally, because a reader who has picked
    /// the pointer up again is looking at where the pointer is.
    pub(super) fn point_at(&mut self, index: usize, position: Point<Pixels>) -> bool {
        if self.pointer == Some(position) {
            return false;
        }

        self.pointer = Some(position);

        if self.selected == index {
            return false;
        }

        self.selected = index;
        true
    }
}

impl AgentPane {
    /// The prompts waiting behind the running turn, one row each, above the
    /// composer. A row whose backend named it carries a control that drops it
    /// again; one this side is only remembering does not, because there is
    /// nothing on the backend such a control could address.
    pub(super) fn render_queued_prompts(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        if self.turn.queued_user_messages.is_empty() {
            return None;
        }

        Some(
            v_flex()
                .w_full()
                .px_3()
                .py_1p5()
                .gap_0p5()
                .border_b_1()
                .border_color(cx.theme().border.opacity(0.6))
                .bg(cx.theme().muted.opacity(0.3))
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .children(self.turn.queued_user_messages.iter().enumerate().map(
                    |(index, prompt)| {
                        h_flex()
                            .w_full()
                            .gap_1()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(queued_message_label(prompt)),
                            )
                            .children(prompt.id.clone().map(|id| {
                                Button::new(("queued-prompt-remove", index))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Close)
                                    .tooltip(i18n("agent-history-queued-remove"))
                                    .aria_label(i18n("agent-history-queued-remove"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_queued_prompt(&id, cx)
                                    }))
                            }))
                    },
                )),
        )
    }

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
    pub(super) fn render_history(
        &self,
        pane_background: Hsla,
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
                .id("agent-history-rows")
                .relative()
                .w_full()
                .h(body_height)
                .flex_none()
                .overflow_hidden()
                .px_2()
                // The highlight is drawn for a pointer over the strip even
                // while a search is being typed, where the arrow keys belong
                // to the input and the keyboard has no highlight of its own.
                // The last pointer position goes with it: a pointer that left
                // and came back to the same place has moved.
                .on_hover(cx.listener(|this, inside: &bool, _, cx| {
                    if this.history_ui.pointer_inside == *inside {
                        return;
                    }

                    this.history_ui.pointer_inside = *inside;
                    if !*inside {
                        this.history_ui.pointer = None;
                    }
                    cx.notify();
                }))
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
                                && let Some(session) = this.runtime.backend.as_mut()
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
                if this.history_ui.mode.dismisses_on_outside_click() {
                    this.history_ui.mode = RecentSessionsMode::Hidden;
                    cx.notify();
                }
            }))
            .child(
                v_flex()
                    .w(relative(0.95))
                    .rounded_t(UI_RADIUS)
                    .border_1()
                    .border_b_0()
                    .border_color(cx.theme().border.opacity(0.6))
                    // Composited over the pane rather than taken at full
                    // alpha: Fluent's `muted` is a translucent overlay tint
                    // (#00000006), so forcing its alpha to 1 would paint the
                    // strip in the tint's bare RGB - solid black in the light
                    // theme, solid white in the dark one. Blending yields the
                    // intended slightly deeper surface under either idiom,
                    // and is a no-op for themes whose `muted` is opaque.
                    .bg(pane_background.blend(cx.theme().muted))
                    .pb(px(20.))
                    .child(
                        h_flex()
                            .w_full()
                            .px_4()
                            .pt_2()
                            .pb_1()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(i18n("agent-history-recent-sessions")),
                            )
                            .child(
                                Checkbox::new("history-scope")
                                    .label(i18n("agent-history-show-all-sessions"))
                                    .checked(self.history_ui.scope == SessionScope::AllDirectories)
                                    .tooltip(i18n("agent-history-show-all-sessions-tooltip"))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.toggle_history_scope(cx)),
                                    ),
                            ),
                    )
                    .child(body),
            )
    }

    /// The directory a listed conversation ran in, when that is not this
    /// tab's. A row from this tab's own directory says nothing by repeating
    /// it, so only the ones that will open elsewhere carry it.
    fn foreign_directory(&self, session: &SessionSummary) -> Option<String> {
        let cwd = session.cwd.as_deref()?;

        (!directories_match(Some(cwd), self.working_directory().as_deref()))
            .then(|| directory_label(cwd))
    }

    /// One history row: title, branch, and relative time, in the settings
    /// row's ghost-control idiom (small, muted, hover lifts the foreground).
    fn render_history_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(session) = self.history_ui.sessions.get(index) else {
            return div().into_any_element();
        };
        // The strip's own surface is the muted tint, so a row state derived
        // from `muted` again lands on the color it sits on and disappears.
        // The list tokens are the per-theme fills meant to read against a
        // surface, translucent in Fluent and in the Modern themes alike.
        //
        // One fill, for the one current row. The pointer and the arrow keys
        // move the same highlight, so a hover tint on top of it would be a
        // second mark for a state the list only has one of.
        let selected = self.history_ui.selected == index
            && matches!(
                self.history_ui.mode,
                RecentSessionsMode::Automatic | RecentSessionsMode::Open
            )
            && (self.history_ui.pointer_inside || self.input.read(cx).text().len() == 0);

        h_flex()
            .id(("history-row", index))
            .h(px(Self::HISTORY_ROW_HEIGHT))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .rounded(UI_RADIUS)
            .cursor_pointer()
            .when(selected, |this| this.bg(cx.theme().list_active))
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                if this.history_ui.point_at(index, event.position) {
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| this.resume_session(index, cx)))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .items_baseline()
                    .child(
                        div()
                            .flex_none()
                            .max_w(relative(0.5))
                            .truncate()
                            .text_sm()
                            .text_color(cx.theme().foreground.opacity(0.82))
                            .child(session.title.clone()),
                    )
                    // A search excerpt is why this row is on screen at all, so
                    // it shares the title's line rather than adding a second
                    // one that would change the list's fixed row height.
                    .children(session.snippet.clone().map(|snippet| {
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground.opacity(0.7))
                            .child(snippet.lines().collect::<Vec<_>>().join(" "))
                    })),
            )
            // Where the conversation ran, on rows that ran somewhere else.
            // Clicking one opens it there rather than continuing it here, so
            // the directory is the row's most load-bearing detail.
            .children(self.foreign_directory(session).map(|directory| {
                h_flex()
                    .flex_none()
                    .gap_1()
                    .items_center()
                    .max_w(px(180.))
                    .child(
                        Icon::new(IconName::Folder)
                            .size_3()
                            .text_color(cx.theme().muted_foreground.opacity(0.7)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground.opacity(0.7))
                            .child(directory),
                    )
            }))
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
