use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, AsyncApp, Context, FontWeight, Hsla, IntoElement, ListSizingBehavior,
    MouseMoveEvent, Pixels, Point, ScrollStrategy, WeakEntity, div, px, relative, size,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::Scrollbar;
use gpui_component::skeleton::Skeleton;
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, VirtualListScrollHandle, h_flex, v_flex,
    v_virtual_list,
};
use nmt_agent::chat::{SessionScope, SessionSummary};
use nmt_agent::session::history::{
    CountPublication, SessionHistory, count_scoped_sessions, list_scoped_sessions,
};
use rust_i18n::t;

use crate::agent_tab::commands::move_palette_selection;
use crate::agent_tab::fade::Fade;
use crate::agent_tab::session::history::FilesystemHistoryRequest;
use crate::agent_tab::session::{directories_match, directory_label};
use crate::agent_tab::settings::UI_RADIUS;
use crate::agent_tab::transcript::relative_time;
use crate::agent_tab::{AgentPane, PaletteControl};
use crate::platform_style::{Host, PlatformStyle as _};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum RecentSessionsMode {
    #[default]
    Automatic,
    Hidden,
    Open,
    Loading,
}

impl RecentSessionsMode {
    /// The automatic list is a blank tab's default surface, and a composer
    /// with anything in it -- typed text or the placeholder a pasted image
    /// leaves -- means the tab is being used for a new conversation, so the
    /// list steps aside and comes back once the composer is empty again. An
    /// explicit `/resume` list stays up over text: typing into it narrows
    /// the rows.
    pub(crate) fn is_visible(
        self,
        transcript_empty: bool,
        composer_empty: bool,
        rows: usize,
    ) -> bool {
        rows > 0
            && match self {
                Self::Automatic => transcript_empty && composer_empty,
                Self::Open => true,
                Self::Hidden | Self::Loading => false,
            }
    }

    /// An outside click dismisses only an explicit `/resume` list. The
    /// automatic list on a blank tab is that tab's default surface, so a
    /// click on the empty pane keeps it open; hiding it there would strand
    /// the tab with no way back except `/resume`.
    pub(crate) fn dismisses_on_outside_click(self) -> bool {
        !matches!(self, Self::Automatic)
    }
}

/// Recent-session list shown above the composer.
pub(crate) struct SessionHistoryUi {
    pub(crate) data: SessionHistory,

    /// Blank conversations show the list automatically; `/resume` can reopen
    /// the same list after a conversation has started.
    pub(crate) mode: RecentSessionsMode,

    /// The one highlighted row, whether the pointer or the arrow keys put it
    /// there. A list has a single current row: what a click opens and what
    /// Enter opens are the same row, and only one thing on screen says so.
    pub(crate) selected: usize,

    /// Whether the pointer is over the list. A search narrows the rows while
    /// the arrow keys still belong to the input, so the keyboard's highlight
    /// is not drawn then; a pointer over the list is reason enough to draw it,
    /// because the row under the pointer is what a click would open.
    pub(crate) pointer_inside: bool,

    /// Where the pointer last was over the list, so a row sliding under a
    /// pointer that has not moved cannot take the highlight back. Keyboard
    /// navigation scrolls the list, which does exactly that.
    pub(crate) pointer: Option<Point<Pixels>>,

    pub(crate) scroll: VirtualListScrollHandle,
    pub(crate) transcript_blur: Fade,
}

impl Default for SessionHistoryUi {
    fn default() -> Self {
        Self {
            data: SessionHistory::default(),
            mode: RecentSessionsMode::Automatic,
            selected: 0,
            pointer_inside: false,
            pointer: None,
            scroll: VirtualListScrollHandle::new(),
            transcript_blur: Fade::default(),
        }
    }
}

/// What a navigation key did to the list.
pub(crate) enum ListControl {
    /// The list is not on screen or the key is not the list's to take.
    Ignored,
    /// The list took the key and nothing on screen changed.
    Unchanged,
    /// The list took the key and has to be drawn again.
    Changed,
    /// The key opens the conversation at this row.
    Resume(usize),
}

/// Height of one history row; all rows are uniform, which is what lets
/// the virtual list precompute its scroll geometry.
const HISTORY_ROW_HEIGHT: f32 = 32.0;

/// Ten rows remain visible; older sessions scroll within this viewport.
const HISTORY_MAX_HEIGHT: f32 = HISTORY_ROW_HEIGHT * 10.0;

impl SessionHistoryUi {
    /// Rows the list lays out: the reserved placeholders while a count result
    /// waits for its entries, the entries themselves once they arrive.
    pub(crate) fn rows(&self) -> usize {
        self.data.pending.unwrap_or(self.data.sessions.len())
    }

    pub(crate) fn is_visible(&self, transcript_empty: bool, composer_empty: bool) -> bool {
        self.mode
            .is_visible(transcript_empty, composer_empty, self.rows())
    }

    /// Show the list on request, highlighting its first row. Returns false,
    /// leaving the list hidden, when there is nothing to list.
    pub(crate) fn open(&mut self) -> bool {
        if self.rows() == 0 {
            self.mode = RecentSessionsMode::Hidden;

            return false;
        }

        self.mode = RecentSessionsMode::Open;
        self.selected = 0;

        // A list opened from a command was opened without the pointer, and a
        // strip that was on screen the last time the pointer crossed it has
        // no way to report that the pointer has since left.
        self.pointer_inside = false;
        self.pointer = None;

        true
    }

    pub(crate) fn publish_filesystem_count(
        &mut self,
        request: &FilesystemHistoryRequest,
        cwd: Option<&str>,
        epoch: u64,
        count: usize,
    ) -> CountPublication {
        let result = self
            .data
            .publish_filesystem_count(request, cwd, epoch, count);

        if matches!(result, CountPublication::Empty) {
            self.selected = 0;
        }

        result
    }

    pub(crate) fn publish_filesystem_rows(
        &mut self,
        request: &FilesystemHistoryRequest,
        cwd: Option<&str>,
        epoch: u64,
        rows: Vec<SessionSummary>,
    ) -> bool {
        if !self.data.publish_filesystem_rows(request, cwd, epoch, rows) {
            return false;
        }

        self.selected = self
            .selected
            .min(self.data.sessions.len().saturating_sub(1));

        true
    }

    /// Hand the highlight to the row under the pointer, reporting whether the
    /// highlight moved.
    ///
    /// Guarded on the pointer having actually moved. Keyboard navigation
    /// scrolls the list to keep its row in view, which slides a different row
    /// under a pointer resting over the strip; letting that count as pointing
    /// would take the highlight straight back off the arrow keys. Real
    /// movement takes it back unconditionally, because a reader who has picked
    /// the pointer up again is looking at where the pointer is.
    pub(crate) fn point_at(&mut self, index: usize, position: Point<Pixels>) -> bool {
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

    /// Widen the list to every directory, or narrow it back to the tab's own.
    /// The rows on screen answered the previous scope, so they go. Returns the
    /// new scope, which the list has to be reloaded for.
    pub(crate) fn toggle_scope(&mut self) -> SessionScope {
        self.data.invalidate_filesystem_history();

        self.data.scope = match self.data.scope {
            SessionScope::CurrentDirectory => SessionScope::AllDirectories,
            SessionScope::AllDirectories => SessionScope::CurrentDirectory,
        };

        self.data.sessions.clear();

        self.data.showing_search = false;
        self.selected = 0;

        self.data.scope
    }

    /// Read the list from the harness's transcript directory for `cwd`, off
    /// the pane's thread, as of session `epoch`. A cheap count comes first so
    /// the list can reserve its final height with placeholder rows, then title
    /// parsing swaps in the real rows.
    pub(crate) fn load_filesystem_history(
        &mut self,
        cwd: Option<String>,
        epoch: u64,
        cx: &mut Context<AgentPane>,
    ) {
        let scope = self.data.scope;

        let request = self.data.begin_filesystem_history(cwd.clone(), epoch);

        cx.notify();

        cx.spawn(async move |this, cx| load_history_passes(this, request, scope, cwd, cx).await)
            .detach();
    }

    /// Answer a navigation key while the list is on screen. The composer has
    /// the keys while it holds text, and completion always belongs to the
    /// command palette.
    pub(crate) fn handle_control(
        &mut self,
        control: PaletteControl,
        transcript_empty: bool,
        composer_empty: bool,
    ) -> ListControl {
        if matches!(control, PaletteControl::Complete)
            || !composer_empty
            || !self.is_visible(transcript_empty, composer_empty)
        {
            return ListControl::Ignored;
        }

        match control {
            PaletteControl::Previous | PaletteControl::Next => {
                let Some(selected) = control.direction().and_then(|direction| {
                    move_palette_selection(self.selected, self.data.sessions.len(), direction)
                }) else {
                    return ListControl::Unchanged;
                };

                self.selected = selected;

                self.scroll
                    .scroll_to_item(selected, ScrollStrategy::Nearest);

                ListControl::Changed
            }
            PaletteControl::Activate => ListControl::Resume(self.selected),
            PaletteControl::Dismiss => {
                self.mode = RecentSessionsMode::Hidden;

                ListControl::Changed
            }
            // Completion belongs to the command palette. The guard above hands
            // it back before the list claims the keys, so there is nothing left
            // for it to do here.
            PaletteControl::Complete => ListControl::Unchanged,
        }
    }

    /// Recent sessions share the composer's width and keep a stable height
    /// while loading, so returning results do not move the input field.
    pub(crate) fn render(
        &self,
        background: Hsla,
        cx: &mut Context<AgentPane>,
    ) -> impl IntoElement + use<> {
        let rows = self.rows();

        let body_height = px((HISTORY_ROW_HEIGHT * rows as f32).min(HISTORY_MAX_HEIGHT));

        let body: AnyElement = if self.data.pending.is_some() {
            // Both loading and loaded bodies use the same explicit viewport
            // height. The virtual list's inferred first-frame measurement
            // must not move the composer when it replaces these placeholders.
            v_flex()
                .w_full()
                .h(body_height)
                .flex_none()
                .px_2()
                .gap_0()
                .children((0..rows.min(3)).map(|i| {
                    h_flex()
                        .h(px(HISTORY_ROW_HEIGHT))
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
            let row_sizes = Rc::new(vec![size(px(0.), px(HISTORY_ROW_HEIGHT)); rows]);

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
                            if visible_range.end >= this.history_ui.data.sessions.len() {
                                this.session.borrow_mut().request_more_history();
                            }

                            let composer_empty = this.input.read(cx).text().len() == 0;
                            let cwd = this.working_directory(cx);

                            visible_range
                                .map(|index| {
                                    this.history_ui.render_row(
                                        index,
                                        composer_empty,
                                        cwd.as_deref(),
                                        cx,
                                    )
                                })
                                .collect()
                        },
                    )
                    .track_scroll(&self.scroll)
                    .with_sizing_behavior(ListSizingBehavior::Infer),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(px(16.))
                        .child(Scrollbar::vertical(&self.scroll)),
                )
                .into_any_element()
        };

        // The picker shares the composer width and leaves a visible gap above it.
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
                    .w_full()
                    .map(|strip| Host::history_strip(strip, background, cx))
                    .pb(px(2.))
                    .child(
                        h_flex()
                            .w_full()
                            .px_2()
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
                                    .child(t!("agent-history-recent-sessions")),
                            )
                            .child(
                                Button::new("history-scope")
                                    .ghost()
                                    .small()
                                    .label(
                                        t!(if self.data.scope == SessionScope::AllDirectories {
                                            "agent-history-all-directories"
                                        } else {
                                            "agent-history-current-directory"
                                        })
                                        .into_owned(),
                                    )
                                    .tooltip(t!("agent-history-show-all-sessions-tooltip"))
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.toggle_history_scope(cx)),
                                    ),
                            ),
                    )
                    .child(body),
            )
    }

    /// One history row: title, branch, and relative time, in the settings
    /// row's ghost-control idiom (small, muted, hover lifts the foreground).
    /// `composer_empty` is whether the composer holds nothing, and `cwd` is
    /// the tab's working directory.
    fn render_row(
        &self,
        index: usize,
        composer_empty: bool,
        cwd: Option<&str>,
        cx: &mut Context<AgentPane>,
    ) -> AnyElement {
        let Some(session) = self.data.sessions.get(index) else {
            return div().into_any_element();
        };

        // One fill, for the one current row. The pointer and the arrow keys
        // move the same highlight, so a hover tint on top of it would be a
        // second mark for a state the list only has one of.
        let selected = self.selected == index
            && matches!(
                self.mode,
                RecentSessionsMode::Automatic | RecentSessionsMode::Open
            )
            && (self.pointer_inside || composer_empty);

        h_flex()
            .id(("history-row", index))
            .h(px(HISTORY_ROW_HEIGHT))
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
                            .max_w(relative(1.0))
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
            .children(foreign_directory(session, cwd).map(|directory| {
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

/// The directory a listed conversation ran in, when that is not this tab's
/// `cwd`. A row from this tab's own directory says nothing by repeating it, so
/// only the ones that will open elsewhere carry it.
fn foreign_directory(session: &SessionSummary, cwd: Option<&str>) -> Option<String> {
    let session_cwd = session.cwd.as_deref()?;

    (!directories_match(Some(session_cwd), cwd)).then(|| directory_label(session_cwd))
}

/// The two off-thread passes of a filesystem history read for `this` pane:
/// the count that reserves the list's height, then the rows themselves.
async fn load_history_passes(
    this: WeakEntity<AgentPane>,
    request: FilesystemHistoryRequest,
    scope: SessionScope,
    cwd: Option<String>,
    cx: &mut AsyncApp,
) {
    let count_cwd = cwd.clone();

    let count = cx
        .background_executor()
        .spawn(async move { count_scoped_sessions(scope, count_cwd.as_deref()) })
        .await;

    let proceed = this
        .update(cx, |this, cx| {
            let cwd = this.cwd(cx);

            match this.history_ui.publish_filesystem_count(
                &request,
                cwd.as_deref(),
                this.session.borrow().runtime().epoch(),
                count,
            ) {
                CountPublication::Stale => false,
                CountPublication::Empty => {
                    cx.notify();

                    false
                }
                CountPublication::LoadRows => {
                    cx.notify();

                    true
                }
            }
        })
        .unwrap_or(false);

    if !proceed {
        return;
    }

    // Title parsing races a short hold: on a warm SSD it finishes
    // within a frame, so without the hold the skeleton rows would
    // never be visible and the swap would read as a flicker.
    let load = cx
        .background_executor()
        .spawn(async move { list_scoped_sessions(scope, cwd.as_deref()) });

    cx.background_executor()
        .timer(Duration::from_millis(250))
        .await;

    let sessions = load.await;

    let _ = this.update(cx, |this, cx| {
        let cwd = this.cwd(cx);

        if this.history_ui.publish_filesystem_rows(
            &request,
            cwd.as_deref(),
            this.session.borrow().runtime().epoch(),
            sessions,
        ) {
            cx.notify();
        }
    });
}
