use std::ops::Range;
use std::path::PathBuf;
use std::time::{self, Duration};

use futures::StreamExt;
use gpui::prelude::*;
use gpui::{
    App, AppContext, Bounds, Context, Entity, EntityInputHandler, EventEmitter, ExternalPaths,
    FocusHandle, Focusable, IntoElement, KeyDownEvent, Keystroke, ListOffset, Modifiers,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollDelta,
    ScrollWheelEvent, Size, UTF16Selection, Window, actions, div, list, point, px, rgb, size,
};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, WindowExt as _};
use nmt_agent_utils::{AgentRoute, agent_process};
use nmt_config::local_state::TabState;
use nmt_config::{CursorShape, active_colors};
use nmt_terminal::block_store::BlockStore;
use nmt_terminal::ghostty::{BlockHandle, ScrollbarInfo};
use nmt_terminal::selection::SelectionType;
use tracing::{info, warn};

use super::frame::{
    TerminalFrame, TerminalFrameCache, theme_default_background, theme_default_foreground,
};
use super::surface::TerminalSurface;
use super::{input, metrics, wake};
use crate::terminal;
use crate::terminal::block_list::{
    BlockListMeasureKey, BlockListPoint, BlockListState, FrozenPoint, ListReconcile,
    RemeasureScope, block_list_active_top_px, block_list_alignment, block_list_render_metrics,
    block_pad_rows, offset_frozen_chrome, plan_list_reconcile, plan_remeasure,
    shift_selected_item_for_eviction,
};
use crate::terminal::dirty::DirtyState;
use crate::terminal::element::{
    BlockListItem, BlockListView, TerminalView, bottom_anchor_offsets, frame_content_rows,
    live_frame_text, row_y_offset, terminal_row_at_y,
};
use crate::terminal::links::LinkHit;
use crate::terminal::scrollbar::{
    SCROLLBAR_LINGER, scrollbar_element, scrollbar_offset_for_thumb, scrollbar_opacity,
};
use crate::terminal::session::{HostEvent, InFlightBlock};
use crate::terminal::surface::{
    SurfaceCell, SurfaceCellSide, SurfaceMouseButton, SurfaceMouseEventKind, SurfaceScreenCell,
    TerminalKeyAction as SurfaceKeyAction, TerminalKeyResult as SurfaceKeyResult,
};
use crate::ui::{AppSettings, surface_background_opacity};

actions!(
    terminal,
    [
        /// Send Tab to the PTY (shell completion).
        SendTab,
        /// Send Shift-Tab to the PTY (shell backward completion).
        SendShiftTab,
        /// Copy the selected command block's command line.
        CopyBlockCommand,
        /// Copy the selected command block's output text.
        CopyBlockOutput,
        /// Re-run the selected command block's command.
        RerunBlock,
        /// Scroll the viewport to the previous command block's start.
        PreviousBlock,
        /// Scroll the viewport to the next command block's start.
        NextBlock,
    ]
);

struct TextCopiedNotification;

fn show_text_copied(window: &mut Window, cx: &mut App) {
    window.push_notification(
        Notification::new()
            .message("Text copied")
            .id::<TextCopiedNotification>()
            .autohide_after(Duration::from_millis(1500))
            .show_close(false)
            .w_auto()
            .px_3()
            .py_2(),
        cx,
    );
}

pub(crate) struct TerminalPane {
    pub(crate) focus: FocusHandle,
    /// Surface/tab id (same value as this pane's `TabId`); the shell pump uses it
    /// to route host events to the owning tab.
    id: u64,
    profile_name: String,
    agent_route: AgentRoute,
    pub(super) surface: TerminalSurface,
    cursor_shape: CursorShape,
    frame_cache: TerminalFrameCache,
    pub(super) cell_metrics: Option<metrics::CellMetrics>,
    /// The terminal leaf's laid-out content rect (window coords, padding
    /// excluded), set from the element's paint. Resize and pointer hit-testing use
    /// it so chrome (tab bar) offsets are honored instead of assuming the window.
    pub(super) content_bounds: Option<Bounds<Pixels>>,
    /// True while the scrollbar thumb is being dragged (mouse-move then scrolls
    /// to the pointer instead of selecting text).
    pub(super) scrollbar_dragging: bool,
    /// Last user scroll action; the scrollbar shows only while dragging or
    /// within [`SCROLLBAR_LINGER`] of this instant, then auto-hides.
    pub(super) last_scroll_activity: Option<time::Instant>,
    /// Bumped per scroll action so only the newest hide-timer repaints.
    scroll_activity_gen: u64,
    /// Pointer offset inside the thumb at drag start (track fraction), so
    /// grabbing the thumb doesn't jump it.
    pub(super) scrollbar_grab: f32,
    wake: wake::WakeSignal,
    dirty: DirtyState,
    /// The in-flight command mirrored from the session on drain.
    in_flight: Option<InFlightBlock>,
    /// Whether a trusted prompt input region is open.
    open_prompt: bool,
    pub(super) block_list: BlockListState,
    /// Hit-test data recorded from the last native list prepaint.
    pub(super) frozen_hit: terminal::block_list::FrozenHitInfo,
    /// Inputs that affect measured heights of the mutable tail of the native list.
    last_list_measure_key: Option<BlockListMeasureKey>,
    /// The gutter-selected frozen item (block-split): highlighted and
    /// targeted by the copy/re-run/jump actions in list mode.
    pub(super) selected_frozen_item: Option<usize>,
    /// Visible frozen item chrome recorded from native list item bounds.
    pub(super) frozen_chrome: Vec<terminal::block_list::FrozenItemChrome>,
    /// Frozen-region selection: (anchor, head), both inclusive cell points.
    frozen_selection: Option<(
        terminal::block_list::FrozenPoint,
        terminal::block_list::FrozenPoint,
    )>,
    /// Visible separator y positions, painted outside GPUI List's content mask.
    pub(super) frozen_separators: Vec<f32>,
    /// Anchor of an in-progress frozen-region drag. The selection itself is
    /// only created on the first mouse-move, so a plain click selects nothing
    /// (matching the engine's empty-selection-dropped-on-up semantics).
    frozen_select_anchor: Option<terminal::block_list::FrozenPoint>,
    /// Pixel origin of a text-selection gesture. Ignoring movement within a
    /// quarter-cell radius prevents normal hand jitter from selecting a glyph.
    selection_drag_origin: Option<Point<Pixels>>,
    /// The link under a Ctrl-hover, underlined until the pointer or the
    /// modifier leaves it.
    pub(super) hovered_link: Option<LinkHit>,
    /// Last pointer position, so a Ctrl press/release without movement can
    /// still update the hover underline.
    pub(super) last_mouse_position: Option<Point<Pixels>>,
}

pub(crate) struct AgentInterrupted;

impl EventEmitter<AgentInterrupted> for TerminalPane {}

impl TerminalPane {
    pub(crate) fn spawn(
        cx: &mut impl AppContext,
        surface_id: u64,
        tab_state: Option<TabState>,
        default_profile: (Option<String>, Vec<String>),
    ) -> Result<Entity<Self>, String> {
        // A `None` shell means "follow the default profile" (session-persistence);
        // filling it here keeps the hardcoded built-in fallback in the session layer
        // from swallowing the configured profile.
        let mut tab_state = tab_state.unwrap_or_default();

        if tab_state.shell.is_none() {
            tab_state.shell = default_profile.0;
            tab_state.args = default_profile.1;
        }

        let profile_name = cx.read_global(|settings: &AppSettings, _| {
            settings.profile_name_for_command(tab_state.shell.as_deref(), &tab_state.args)
        });

        let (wake, wake_rx) = wake::wake_channel();
        let agent_route = agent_process().allocate_route();
        let environment = agent_process().environment_for(&agent_route);
        let (fixed_bottom_requested, cursor_shape) = cx.read_global(|settings: &AppSettings, _| {
            (
                settings.input_style.is_fixed_bottom(),
                settings.cursor_shape,
            )
        });

        let surface = terminal_surface_for_tab(
            &wake,
            surface_id,
            &tab_state,
            &profile_name,
            cursor_shape,
            environment,
        )?;

        Ok(cx.new(|cx| {
            Self::from_surface(
                cx,
                surface_id,
                profile_name,
                agent_route,
                wake,
                wake_rx,
                surface,
                fixed_bottom_requested,
                cursor_shape,
            )
        }))
    }

    /// Spawn a pane backed by an already-attached remote session. Mirrors
    /// [`Self::spawn`] but skips local-only concerns (shell profile, working
    /// dir); the remote host owns the process.
    #[cfg(windows)]
    pub(crate) fn spawn_remote(
        cx: &mut impl AppContext,
        surface_id: u64,
        remote: nmt_remote_net::RemoteSession,
    ) -> Result<Entity<Self>, String> {
        let (wake, wake_rx) = wake::wake_channel();
        let agent_route = agent_process().allocate_route();
        let (fixed_bottom_requested, cursor_shape) = cx.read_global(|settings: &AppSettings, _| {
            (
                settings.input_style.is_fixed_bottom(),
                settings.cursor_shape,
            )
        });

        let surface = TerminalSurface::for_gpui_remote(wake.clone(), surface_id, remote)?;

        Ok(cx.new(|cx| {
            Self::from_surface(
                cx,
                surface_id,
                "Remote".to_string(),
                agent_route,
                wake,
                wake_rx,
                surface,
                fixed_bottom_requested,
                cursor_shape,
            )
        }))
    }

    fn from_surface(
        cx: &mut Context<Self>,
        surface_id: u64,
        profile_name: String,
        agent_route: AgentRoute,
        wake: wake::WakeSignal,
        mut wake_rx: wake::WakeReceiver,
        surface: TerminalSurface,
        fixed_bottom_requested: bool,
        cursor_shape: CursorShape,
    ) -> Self {
        // Apply terminal presentation settings to existing panes and invalidate
        // measurements that depend on font metrics.
        cx.observe_global::<AppSettings>(|this, cx| {
            let settings = cx.global::<AppSettings>();
            let fixed_bottom = settings.input_style.is_fixed_bottom();
            let cursor_shape = settings.cursor_shape;

            this.block_list
                .list
                .set_alignment(block_list_alignment(fixed_bottom));

            this.surface.set_theme_colors(&active_colors());

            if cursor_shape != this.cursor_shape && this.surface.set_cursor_shape(cursor_shape) {
                this.cursor_shape = cursor_shape;
            }

            this.cell_metrics = None;

            this.frame_cache.invalidate_full();

            cx.notify();
        })
        .detach();

        cx.spawn(async move |this, cx| {
            while let Some(wake) = wake_rx.next().await {
                if this
                    .update(cx, |this, cx| match wake {
                        wake::Wake::Content(_) => this.invalidate(cx),
                        wake::Wake::Chrome(_) => this.invalidate_chrome(cx),
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        Self {
            focus: cx.focus_handle(),
            id: surface_id,
            profile_name,
            agent_route,
            surface,
            cursor_shape,
            frame_cache: TerminalFrameCache::default(),
            cell_metrics: None,
            content_bounds: None,
            scrollbar_dragging: false,
            last_scroll_activity: None,
            scroll_activity_gen: 0,
            scrollbar_grab: 0.0,
            wake,
            dirty: DirtyState::default(),
            in_flight: None,
            open_prompt: false,
            block_list: BlockListState::new(block_list_alignment(fixed_bottom_requested)),
            frozen_hit: Default::default(),
            last_list_measure_key: None,
            selected_frozen_item: None,
            frozen_chrome: Vec::new(),
            frozen_selection: None,
            frozen_separators: Vec::new(),
            frozen_select_anchor: None,
            selection_drag_origin: None,
            hovered_link: None,
            last_mouse_position: None,
        }
    }

    pub(crate) fn agent_route(&self) -> &AgentRoute {
        &self.agent_route
    }

    pub(crate) fn profile_name(&self) -> &str {
        &self.profile_name
    }

    /// Record a user scroll action and schedule the repaint that starts fading
    /// the scrollbar once [`SCROLLBAR_LINGER`] passes without further activity.
    pub(super) fn mark_scroll_activity(&mut self, cx: &mut Context<Self>) {
        self.last_scroll_activity = Some(time::Instant::now());
        self.scroll_activity_gen += 1;

        let generation = self.scroll_activity_gen;

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SCROLLBAR_LINGER).await;

            let _ = this.update(cx, |this, cx| {
                // Stale timers from earlier scroll ticks no-op; only the newest
                // one repaints (with the linger expired, starting fade-out).
                if this.scroll_activity_gen == generation && !this.scrollbar_dragging {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn cell_metrics(&mut self, window: &mut Window, cx: &App) -> metrics::CellMetrics {
        *self
            .cell_metrics
            .get_or_insert_with(|| metrics::measure_cell(window, cx))
    }

    /// Top-left of the terminal content in window coords (falls back to origin
    /// before the first paint).
    pub(super) fn content_origin(&self) -> Point<Pixels> {
        self.content_bounds
            .map(|bounds| bounds.origin)
            .unwrap_or_default()
    }

    /// Store the terminal leaf's laid-out content rect and resize the surface to
    /// it. Called from the element's paint, where actual bounds are known.
    pub(crate) fn set_content_bounds(
        &mut self,
        bounds: Bounds<Pixels>,
        cell: metrics::CellMetrics,
        cx: &mut Context<Self>,
    ) {
        self.content_bounds = Some(bounds);

        if self.surface.resize_for_content(
            bounds.size.width.as_f32(),
            bounds.size.height.as_f32(),
            cell,
        ) {
            self.frame_cache.invalidate();
            cx.notify();
        }
    }

    fn refresh_frame(&mut self) {
        let previous = self.frame_cache.reusable_frame();

        self.frame_cache
            .rebuild(self.surface.frame(previous.as_ref()));
    }

    fn invalidate(&mut self, cx: &mut Context<Self>) {
        self.frame_cache.invalidate();

        if self.dirty.mark() {
            cx.notify();
        }
    }

    fn invalidate_chrome(&mut self, cx: &mut Context<Self>) {
        self.frame_cache.invalidate();

        self.dirty.mark();

        // Background panes cannot clear their dirty bit by rendering, but the
        // shell observer still needs every chrome wake to refresh tab state.
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if should_scroll_to_latest(&event.keystroke, self.surface.alt_screen())
            && self.scroll_to_latest(cx)
        {
            return;
        }

        // Plain printable text arrives through the char/IME path
        // (`replace_text_in_range`); encoding it here too would double it.
        if input::should_defer_to_ime(&event.keystroke) {
            return;
        }

        let action = input::key_action(&event.keystroke);
        let interrupts_agent = matches!(event.keystroke.key.as_str(), "escape" | "esc")
            && !event.keystroke.modifiers.modified();

        // Block-split: copy the frozen-region selection on the copy chord.
        if let (SurfaceKeyAction::CopyOrWrite(_), Some((a, b))) = (&action, self.frozen_selection) {
            let text = self.frozen_selection_to_text(a, b);
            if !text.is_empty() {
                self.surface.copy_text_to_clipboard(text);
                show_text_copied(window, cx);
                self.frozen_selection = None;
                cx.notify();
                return;
            }
        }

        let result = self.surface.apply_key_action(action);

        if result == SurfaceKeyResult::Copied {
            show_text_copied(window, cx);
        }

        if result != SurfaceKeyResult::Ignored {
            if interrupts_agent {
                cx.emit(AgentInterrupted);
            }

            self.invalidate(cx);
        }
    }

    /// Route a keystroke straight to the terminal PTY.
    pub(crate) fn feed_terminal_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        if self.surface.apply_key_action(input::key_action(keystroke)) != SurfaceKeyResult::Ignored
        {
            self.invalidate(cx);
        }
    }

    /// Tab/Shift-Tab belong to the shell (completion) while the terminal is
    /// focused, but `Root` binds them to focus traversal and key bindings
    /// dispatch before the pane's `on_key_down` listener. These actions are
    /// bound in the deeper `Terminal` context, which wins over `Root`.
    fn on_send_tab(&mut self, _: &SendTab, _: &mut Window, cx: &mut Context<Self>) {
        self.feed_terminal_key(
            &Keystroke {
                modifiers: Modifiers::none(),
                key: "tab".into(),
                key_char: None,
            },
            cx,
        );
    }

    fn on_send_shift_tab(&mut self, _: &SendShiftTab, _: &mut Window, cx: &mut Context<Self>) {
        self.feed_terminal_key(
            &Keystroke {
                modifiers: Modifiers::shift(),
                key: "tab".into(),
                key_char: None,
            },
            cx,
        );
    }

    fn on_file_drop(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);

        if self.surface.paste_text(&dropped_paths_text(paths.paths())) {
            self.invalidate(cx);
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);

        self.selection_drag_origin = None;

        // Ctrl+left-click opens the URL under the pointer (OSC 8 target or
        // URL-shaped text). It wins over selection and mouse reporting so
        // links stay clickable inside TUIs, matching common terminal behavior.
        if event.button == MouseButton::Left
            && event.modifiers.control
            && !event.modifiers.alt
            && !event.modifiers.shift
        {
            if let Some(link) = self.link_at_position(event.position, cx) {
                info!(url = link.url, "ctrl+click open url");
                cx.open_url(&link.url);
                return;
            }
        }

        // A left-click on a block-list gutter selects the item instead of
        // starting a text selection.
        if event.button == MouseButton::Left && self.block_chrome_enabled(cx) {
            if self.try_select_frozen_item(event.position, cx) {
                return;
            }
        }

        // Block-split: a left press in the frozen region starts a frozen
        // selection (and drops the engine one); any other press clears it.
        let reports_mouse = self
            .surface
            .mouse_reporting_active_for(input::modifiers_state(event.modifiers));

        let selection_type = selection_type_for_click_count(event.click_count);

        self.selection_drag_origin =
            (event.button == MouseButton::Left && !reports_mouse).then_some(event.position);

        if self.block_list_mode(cx) && !reports_mouse {
            if event.button == MouseButton::Left {
                if let Some(BlockListPoint::Frozen(pt)) =
                    self.block_list_point_at(event.position, cx)
                {
                    // The engine highlight is baked into the cached frame, so
                    // clearing the selection needs a frame rebuild too.
                    self.surface.clear_selection();

                    if selection_type == SelectionType::Simple {
                        self.frozen_selection = None;
                        self.frozen_select_anchor = Some(pt);
                    } else {
                        self.frozen_selection = self.expanded_frozen_selection(pt, selection_type);
                        self.frozen_select_anchor = None;
                    }

                    self.invalidate(cx);

                    cx.notify();

                    return;
                }
            }

            if self.frozen_selection.take().is_some() {
                cx.notify();
            }
        }

        self.apply_mouse_event(
            event.position,
            Some(event.button),
            SurfaceMouseEventKind::Down,
            event.modifiers,
            selection_type,
            window,
            cx,
        );
    }

    /// Map a window-y pointer position to a 0..1 fraction of the content height.
    pub(super) fn scrollbar_fraction(&self, y: Pixels) -> f32 {
        let bounds = self.content_bounds.unwrap_or_default();
        let height = bounds.size.height.as_f32().max(1.0);

        ((y.as_f32() - bounds.origin.y.as_f32()) / height).clamp(0.0, 1.0)
    }

    /// Restore the newest output only when the viewport has actually moved,
    /// leaving End available for normal shell line navigation at the bottom.
    fn scroll_to_latest(&mut self, cx: &mut Context<Self>) -> bool {
        if self.block_list_mode(cx) {
            let (offset, max) = self.block_list.scrollbar;

            if offset >= max {
                return false;
            }

            self.block_list.list.scroll_to_end();
            self.block_list.scrollbar.0 = max;

            self.mark_scroll_activity(cx);

            cx.notify();

            return true;
        }

        let scrolled = self.frame_cache.current().is_some_and(|frame| {
            let scrollbar = frame.scrollbar();

            scrollbar.offset < scrollbar.total.saturating_sub(scrollbar.len)
        });

        if scrolled {
            self.scroll_thumb_to(1.0, cx);
        }

        scrolled
    }

    /// Scroll so the thumb's top sits at `thumb_top` of the track.
    pub(super) fn scroll_thumb_to(&mut self, thumb_top: f32, cx: &mut Context<Self>) {
        if self.block_list_mode(cx) {
            let (_, max_scroll) = self.block_list.scrollbar;
            let viewport = self
                .content_bounds
                .map(|b| b.size.height.as_f32())
                .unwrap_or(0.0);
            let total = max_scroll + viewport;

            let Some(new) = scrollbar_offset_for_thumb(total as f64, viewport as f64, thumb_top)
            else {
                return;
            };

            let new = new as f32;

            if let (Some(frame), Some(cell)) = (self.frame_cache.current(), self.cell_metrics) {
                let cols = self.content_cols();
                let pad_rows = block_pad_rows(cx);
                let store = self.surface.block_store();
                let store = store.lock();
                let offset =
                    self.list_offset_for_px(&store, &frame, cols, cell.height_px, pad_rows, new);
                self.block_list.list.scroll_to(offset);
                self.block_list.scrollbar.0 = new;
            }

            self.mark_scroll_activity(cx);

            cx.notify();

            return;
        }

        let sb = self
            .frame_cache
            .current()
            .map(|frame| frame.scrollbar())
            .unwrap_or_default();

        let scrollable = sb.total.saturating_sub(sb.len);

        if scrollable == 0 {
            return;
        }

        let target = scrollbar_offset_for_thumb(sb.total as f64, sb.len as f64, thumb_top)
            .unwrap_or_default()
            .round() as u64;

        let delta = target as isize - sb.offset as isize;

        if delta != 0 && self.surface.scroll_lines(delta) {
            self.mark_scroll_activity(cx);

            self.invalidate(cx);
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.selection_drag_origin = None;

        if self.scrollbar_dragging {
            // Drag ended: start the linger countdown that hides the bar.
            self.mark_scroll_activity(cx);
        }

        self.scrollbar_dragging = false;

        if self.frozen_select_anchor.take().is_some() {
            return;
        }

        self.apply_mouse_event(
            event.position,
            Some(event.button),
            SurfaceMouseEventKind::Up,
            event.modifiers,
            SelectionType::Simple,
            window,
            cx,
        );
    }

    pub(super) fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.last_mouse_position = Some(event.position);

        if event.pressed_button.is_none() {
            self.update_hovered_link(event.position, event.modifiers, cx);
        }

        if self.scrollbar_dragging {
            let fraction = self.scrollbar_fraction(event.position.y);
            self.scroll_thumb_to(fraction - self.scrollbar_grab, cx);
            return;
        }

        if let Some(origin) = self.selection_drag_origin {
            let cell_width = self.cell_metrics(window, cx).width_px;

            if !selection_drag_started(origin, event.position, cell_width) {
                return;
            }

            self.selection_drag_origin = None;
        }

        if let Some(anchor) = self.frozen_select_anchor {
            // Clamp into the frozen region so a drag past the boundary sticks to
            // the last frozen row instead of vanishing.
            let mut pos = event.position;

            let origin = self.content_origin();
            let max_y = origin.y + px((self.frozen_hit.active_top - 1.0).max(0.0));

            if pos.y > max_y {
                pos.y = max_y;
            }

            if let Some(BlockListPoint::Frozen(head)) = self.block_list_point_at(pos, cx) {
                self.frozen_selection = Some((anchor, head));

                cx.notify();
            }

            return;
        }

        self.apply_mouse_event(
            event.position,
            event.pressed_button,
            SurfaceMouseEventKind::Move,
            event.modifiers,
            SelectionType::Simple,
            window,
            cx,
        );
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Rows shift under the pointer; drop the underline instead of leaving
        // it stale. The next mouse move recomputes it.
        if self.hovered_link.take().is_some() {
            cx.notify();
        }

        let cell_metrics = self.cell_metrics(window, cx);

        let lines = terminal_scroll_lines(event.delta, cell_metrics);

        if lines == 0 {
            return;
        }

        // Block-split: scrolling is list state; the engine viewport stays
        // pinned. TUI mouse reporting still goes to the program.
        if self.block_list_mode(cx) && !self.surface.mouse_reporting_active() {
            return;
        }

        let offsets = self.current_row_offsets(cx);

        let (cell, _) = terminal_cell_at_position(
            event.position,
            self.content_origin(),
            cell_metrics,
            &offsets,
        );

        if self
            .surface
            .apply_scroll(cell, lines, input::modifiers_state(event.modifiers))
        {
            self.mark_scroll_activity(cx);

            self.invalidate(cx);
        }
    }

    fn apply_mouse_event(
        &mut self,
        position: Point<Pixels>,
        button: Option<MouseButton>,
        kind: SurfaceMouseEventKind,
        modifiers: Modifiers,
        selection_type: SelectionType,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cell_metrics = self.cell_metrics(window, cx);

        let modifiers = input::modifiers_state(modifiers);

        if self.block_list_mode(cx)
            && !self.surface.mouse_reporting_active_for(modifiers)
            && button == Some(MouseButton::Left)
            && let Some(point) = self.block_list_point_at(position, cx)
        {
            let (_, side) =
                terminal_cell_at_position(position, self.content_origin(), cell_metrics, &[]);

            let cell = match point {
                BlockListPoint::LiveHistory { row, col } => SurfaceScreenCell { row, col },
                // An engine selection cannot cross into an immutable finished
                // block, so dragging above the active block clamps to its first
                // SCREEN row.
                BlockListPoint::Frozen(point) => SurfaceScreenCell {
                    row: 0,
                    col: point.col.min(u16::MAX as u32) as u16,
                },
            };

            if self
                .surface
                .apply_screen_selection(cell, side, kind, selection_type)
            {
                self.invalidate(cx);
            }

            return;
        }

        let offsets = self.current_row_offsets(cx);

        // Block-split: the live grid starts at `active_top` in the list, so
        // shift the mapping origin.
        let mut origin = self.content_origin();

        if self.block_list_mode(cx) {
            origin.y += px(self.frozen_hit.active_top);
        }

        let (cell, side) = terminal_cell_at_position(position, origin, cell_metrics, &offsets);

        let handled = self.surface.apply_mouse(
            cell,
            side,
            button.and_then(surface_mouse_button),
            kind,
            modifiers,
            selection_type,
        );

        if handled {
            self.invalidate(cx);
        }
    }

    /// The current frame's row offsets for pointer/IME mapping.
    pub(super) fn current_row_offsets(&self, cx: &App) -> Vec<f32> {
        if self.block_list_mode(cx) {
            return Vec::new();
        }

        let (Some(frame), Some(cell)) = (self.frame_cache.current(), self.cell_metrics) else {
            return Vec::new();
        };

        let fixed_bottom = cx.global::<AppSettings>().input_style.is_fixed_bottom();

        bottom_anchor_offsets(&frame, cell.height_px, fixed_bottom)
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn terminal_title(&self) -> String {
        self.surface.title()
    }

    /// The pane's last laid-out content size (`None` before the first paint).
    /// Split creation uses it to check the focused pane can yield the minimum
    /// panel size.
    pub(crate) fn content_size(&self) -> Option<Size<Pixels>> {
        self.content_bounds.map(|bounds| bounds.size)
    }

    /// Number of child processes in the shell's Job Object (requires the
    /// job-management setting; 0 otherwise).
    pub(crate) fn child_process_count(&self) -> usize {
        self.surface.child_process_count()
    }

    pub(crate) fn tab_state(&self) -> TabState {
        self.surface.tab_state()
    }

    /// Drain queued host events, applying the pane-side effects (read-only on
    /// exit, interactive state, boundary trust) and returning the events so the
    /// shell pump can update chrome (tab title, exited, window title). Runs for
    /// every pane — active or background — driven by the shell's observer.
    pub(crate) fn drain_host_events(&mut self) -> Vec<HostEvent> {
        let events = self.surface.poll_events();

        for event in &events {
            match event {
                HostEvent::Exit => self.surface.mark_read_only(),
                HostEvent::InteractiveState(on) => {
                    info!(interactive = *on, "terminal interactive state changed");
                }
                HostEvent::AltScreen(on) => {
                    self.surface.set_alt_screen(*on);
                }
                HostEvent::PromptBoundaryTrusted(on) => {
                    info!(
                        prompt_boundary_trusted = *on,
                        "terminal prompt boundary trust changed"
                    );
                }
                HostEvent::Cwd(cwd) => self.surface.set_last_cwd(cwd.clone()),
                HostEvent::Title(_)
                | HostEvent::Bell
                | HostEvent::Progress(_)
                | HostEvent::Notification { .. }
                | HostEvent::Diagnostic(_) => {}
                HostEvent::CommandFinished => {
                    // Finishing transfers the active SCREEN rows into an
                    // immutable block, so live selection anchors no longer
                    // address the content they were created for.
                    self.surface.clear_selection();

                    self.frame_cache.invalidate();

                    self.refresh_blocks();
                }
                // Mirror the session's split block state for the render path.
                HostEvent::CommandStarted => {
                    self.refresh_blocks();
                }
                HostEvent::PromptStarted => {
                    self.refresh_blocks();
                }
            }
            if matches!(
                event,
                HostEvent::PromptBoundaryTrusted(false) | HostEvent::Exit
            ) {
                self.refresh_blocks();
            }
        }
        events
    }

    /// Mirror the session's live split state onto the pane.
    fn refresh_blocks(&mut self) {
        self.in_flight = self.surface.in_flight_block();
        self.open_prompt = self.surface.open_prompt_region();
    }

    /// Engine blocks remain the storage model while the setting controls only
    /// their presentation, so display changes never hide frozen output.
    /// Alt-screen stays a plain terminal grid.
    pub(super) fn block_list_mode(&self, _cx: &App) -> bool {
        self.surface.engine_blocks() && !self.surface.alt_screen()
    }

    fn block_chrome_enabled(&self, cx: &App) -> bool {
        self.block_list_mode(cx) && cx.global::<AppSettings>().command_blocks
    }

    /// Columns of the content area (block-split hit-testing).
    fn content_cols(&self) -> u32 {
        match (self.content_bounds, self.cell_metrics) {
            (Some(b), Some(cell)) => {
                (b.size.width.as_f32() / cell.width_px).floor().max(1.0) as u32
            }
            _ => 80,
        }
    }

    fn block_list_total_px(
        &self,
        store: &BlockStore,
        frame: &TerminalFrame,
        cols: u32,
        cell_h: f32,
        pad_rows: f32,
    ) -> f32 {
        let frozen: f32 = store
            .items()
            .iter()
            .map(|item| terminal::block_list::item_px(item, cols, cell_h, pad_rows))
            .sum();

        frozen
            + self.live_history_rows(frame) as f32 * cell_h
            + frame_content_rows(frame) as f32 * cell_h
    }

    /// The active grid's scrollback rows, rendered above the live grid
    /// inside the live item when scrolling into a running command.
    /// 0 in classic single-grid mode (no block list is shown there anyway).
    fn live_history_rows(&self, frame: &TerminalFrame) -> u64 {
        if !self.surface.engine_blocks() {
            return 0;
        }

        let sb = frame.scrollbar();

        sb.total.saturating_sub(sb.len)
    }

    fn list_offset_for_px(
        &self,
        store: &BlockStore,
        frame: &TerminalFrame,
        cols: u32,
        cell_h: f32,
        pad_rows: f32,
        target: f32,
    ) -> ListOffset {
        let mut y = 0.0f32;

        for (ix, item) in store.items().iter().enumerate() {
            let h = terminal::block_list::item_px(item, cols, cell_h, pad_rows);
            if target < y + h {
                return ListOffset {
                    item_ix: ix,
                    offset_in_item: px((target - y).max(0.0)),
                };
            }
            y += h;
        }

        let live_h = self.block_list_total_px(store, frame, cols, cell_h, pad_rows) - y;

        if target < y + live_h {
            ListOffset {
                item_ix: store.items().len(),
                offset_in_item: px((target - y).max(0.0)),
            }
        } else {
            ListOffset {
                item_ix: store.items().len() + 1,
                offset_in_item: px(0.0),
            }
        }
    }

    pub(super) fn begin_block_list_frame(
        &mut self,
        bounds: Bounds<Pixels>,
        cell: metrics::CellMetrics,
        cx: &mut Context<Self>,
    ) {
        self.set_content_bounds(bounds, cell, cx);

        self.frozen_hit.clear();
        self.frozen_hit.set_active_top(self.block_list.active_top);
        self.frozen_chrome.clear();
        self.frozen_separators.clear();
    }

    pub(super) fn record_frozen_view(
        &mut self,
        view: &terminal::block_list::FrozenView,
        item_top: f32,
    ) {
        self.frozen_separators
            .extend(view.separators.iter().map(|y| item_top + y));

        for row in &view.rows {
            self.frozen_hit
                .push_row(item_top + row.y, row.item, row.row, row.cell_count);
        }

        for chrome in &view.items_chrome {
            self.frozen_chrome
                .push(offset_frozen_chrome(chrome.clone(), item_top));
        }
    }

    pub(super) fn record_frozen_chrome(
        &mut self,
        chrome: terminal::block_list::FrozenItemChrome,
        item_top: f32,
    ) {
        self.frozen_chrome
            .push(offset_frozen_chrome(chrome, item_top));
    }

    /// Map a window position to either an immutable block row or an absolute
    /// SCREEN row from the active block's history.
    pub(super) fn block_list_point_at(
        &self,
        position: Point<Pixels>,
        cx: &App,
    ) -> Option<BlockListPoint> {
        if !self.block_list_mode(cx) {
            return None;
        }

        let cell = self.cell_metrics?;
        let origin = self.content_origin();
        let local_x = (position.x - origin.x).as_f32();
        let local_y = (position.y - origin.y).as_f32();

        if local_y >= self.frozen_hit.active_top {
            return None;
        }

        self.frozen_hit.hit_test(
            local_x,
            local_y,
            cell.width_px,
            cell.height_px,
            self.content_cols(),
            block_pad_rows(cx),
        )
    }

    /// Handle a mouse-down as a gutter selection in the block list. Frozen
    /// and live items both report chrome from list item bounds. Returns `true`
    /// when the click selected something; a click elsewhere clears selection
    /// and falls through.
    fn try_select_frozen_item(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        if self.surface.mouse_reporting_active() {
            return false;
        }

        let origin = self.content_origin();

        if block_gutter_hit(position.x.as_f32(), origin.x.as_f32()) {
            let y = (position.y - origin.y).as_f32();

            let hit = self
                .frozen_chrome
                .iter()
                .find(|chrome| (chrome.top..chrome.bottom).contains(&y))
                .map(|chrome| chrome.item);

            if let Some(item) = hit {
                self.selected_frozen_item = Some(item);

                cx.notify();

                return true;
            }
        }

        let cleared_frozen = self.selected_frozen_item.take().is_some();

        if cleared_frozen {
            cx.notify();
        }

        false
    }

    /// Scroll the list so the previous/next non-filler item's top lands at
    /// the viewport top; no-op at the edges (list-mode Previous/NextBlock).
    fn jump_to_frozen_item(&mut self, direction: i8, cx: &mut Context<Self>) {
        let Some(cell) = self.cell_metrics else {
            return;
        };

        let cols = self.content_cols();
        let pad_rows = block_pad_rows(cx);
        let (resolved, max_scroll) = self.block_list.scrollbar;
        let store = self.surface.block_store();

        // One lock hold for both reads: target resolution and offset
        // conversion see the same block-list state.
        let store = store.lock();

        let Some(target) = terminal::block_list::nav_item_top(
            &store,
            cols,
            cell.height_px,
            pad_rows,
            resolved,
            direction,
        ) else {
            return;
        };

        if let Some(frame) = self.frame_cache.current() {
            self.block_list.list.scroll_to(self.list_offset_for_px(
                &store,
                &frame,
                cols,
                cell.height_px,
                pad_rows,
                target,
            ));
        }

        drop(store);

        self.block_list.scrollbar.0 = target.min(max_scroll);

        self.mark_scroll_activity(cx);

        cx.notify();
    }

    /// While a command runs, repaint about once a second so the running block's
    /// elapsed time advances even with no PTY output.
    fn schedule_block_tick(&mut self, cx: &mut Context<Self>) {
        self.block_list.tick_gen += 1;

        let generation = self.block_list.tick_gen;

        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(time::Duration::from_secs(1))
                .await;

            let _ = this.update(cx, |this, cx| {
                if this.block_list.tick_gen == generation && this.in_flight.is_some() {
                    this.invalidate(cx);
                }
            });
        })
        .detach();
    }

    /// Metadata of the selected frozen item (list mode): the command line
    /// when one is known.
    fn selected_frozen_command(&self) -> Option<String> {
        let item = self.selected_frozen_item?;
        let store = self.surface.block_store();
        let store = store.lock();

        if item < store.items().len() {
            return store.items().get(item)?.meta.command.clone();
        }

        if item == store.items().len() {
            return self.in_flight.as_ref().map(|block| block.command.clone());
        }

        None
    }

    fn selected_frozen_output(&self) -> Option<String> {
        let item = self.selected_frozen_item?;
        let store = self.surface.block_store();
        let store = store.lock();

        if item < store.items().len() {
            // Block items format through the engine after the store lock
            // drops (the two locks never nest).
            let handle = store.items().get(item)?.handle()?;

            drop(store);

            return self.format_block_range(handle, None, None);
        }

        let is_live = item == store.items().len();

        drop(store);

        if is_live {
            return self
                .frame_cache
                .current()
                .and_then(|frame| live_frame_text(&frame));
        }

        None
    }

    fn expanded_frozen_selection(
        &self,
        point: FrozenPoint,
        selection_type: SelectionType,
    ) -> Option<(FrozenPoint, FrozenPoint)> {
        // The PTY path acquires engine before store, so release the store
        // guard before asking the surface to inspect the engine-owned block.
        let handle = {
            let store = self.surface.block_store();
            let store = store.lock();
            store.items().get(point.item)?.handle()?
        };

        let ((start_line, start_col), (end_line, end_col)) =
            self.surface
                .frozen_selection_range(handle, point.line, point.col, selection_type)?;

        Some((
            FrozenPoint {
                item: point.item,
                line: start_line,
                col: start_col,
            },
            FrozenPoint {
                item: point.item,
                line: end_line,
                col: end_col,
            },
        ))
    }

    /// Resolve a frozen selection to plain text: the per-block ranges are
    /// collected under the store lock, then formatted through acquired
    /// `BlockRef`s after it is released so the store and engine locks are never nested.
    fn frozen_selection_to_text(
        &self,
        a: terminal::block_list::FrozenPoint,
        b: terminal::block_list::FrozenPoint,
    ) -> String {
        let pieces = {
            let store = self.surface.block_store();
            let store = store.lock();

            terminal::block_list::frozen_selection_pieces(&store, a, b)
        };

        pieces
            .into_iter()
            .filter_map(|piece| self.format_block_range(piece.handle, piece.start, piece.end))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Export an inclusive cell range of a finished engine block as plain
    /// text (`None` endpoints = the block edge). Soft-wrapped lines rejoin
    /// and trailing blanks trim, matching the legacy line-item copy.
    fn format_block_range(
        &self,
        handle: BlockHandle,
        start: Option<(usize, u32)>,
        end: Option<(usize, u32)>,
    ) -> Option<String> {
        self.surface
            .acquire_block(handle)?
            .block
            .format_range_clamped(start, end, true, true)
    }

    fn on_copy_block_command(
        &mut self,
        _: &CopyBlockCommand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(command) = self.selected_frozen_command() {
            self.surface.copy_text_to_clipboard(command);
            cx.notify();
        }
    }

    fn on_copy_block_output(
        &mut self,
        _: &CopyBlockOutput,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = self.selected_frozen_output();

        if let Some(text) = text.filter(|t| !t.is_empty()) {
            self.surface.copy_text_to_clipboard(text);
            cx.notify();
        }
    }

    fn on_rerun_block(&mut self, _: &RerunBlock, _: &mut Window, cx: &mut Context<Self>) {
        let command = self.selected_frozen_command();

        let Some(command) = command else {
            return;
        };

        self.surface.write_text(&command);
        self.surface.write_text("\r");

        self.invalidate(cx);
    }

    fn on_previous_block(&mut self, _: &PreviousBlock, _: &mut Window, cx: &mut Context<Self>) {
        self.jump_to_frozen_item(-1, cx);
    }

    fn on_next_block(&mut self, _: &NextBlock, _: &mut Window, cx: &mut Context<Self>) {
        self.jump_to_frozen_item(1, cx);
    }
}

/// Width of the block gutter hit band / strip, in px left of the content origin
/// (inside the pane's padding).
pub(crate) const BLOCK_GUTTER_WIDTH: f32 = 4.0;
/// Gap between the gutter strip and the text; GAP + WIDTH = PADDING_PX so the
/// strip sits flush against the pane's left edge.
pub(crate) const BLOCK_GUTTER_GAP: f32 = metrics::PADDING_PX - BLOCK_GUTTER_WIDTH;

/// A pointer x hits the block gutter when it falls in the strip painted in the
/// left padding, with a small tolerance into column 0.
fn block_gutter_hit(x: f32, origin_x: f32) -> bool {
    let left = origin_x - BLOCK_GUTTER_GAP - BLOCK_GUTTER_WIDTH - 2.0;
    let right = origin_x + 3.0;
    (left..=right).contains(&x)
}

fn terminal_surface_for_tab(
    wake: &wake::WakeSignal,
    surface_id: u64,
    state: &TabState,
    profile_name: &str,
    cursor_shape: CursorShape,
    environment_overrides: Vec<(String, String)>,
) -> Result<TerminalSurface, String> {
    match TerminalSurface::for_gpui(
        wake.clone(),
        surface_id,
        state.shell.clone(),
        state.args.clone(),
        state.cwd.clone(),
        profile_name.to_string(),
        cursor_shape,
        environment_overrides.clone(),
    ) {
        Ok(surface) => Ok(surface),

        Err(error) if state.cwd.is_some() => {
            warn!("restored tab failed with saved cwd, retrying without cwd: {error}");

            TerminalSurface::for_gpui(
                wake.clone(),
                surface_id,
                state.shell.clone(),
                state.args.clone(),
                None,
                profile_name.to_string(),
                cursor_shape,
                environment_overrides,
            )
        }
        Err(error) => Err(error),
    }
}

impl Focusable for TerminalPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Commit-only IME: composition and candidate placement stay with the OS; the
/// pane receives only the committed string. Inline preedit stays in the IME-owned UI,
/// marked-text methods are inert. `bounds_for_range` reports the terminal cursor
/// cell so the OS positions the candidate window correctly.
impl EntityInputHandler for TerminalPane {
    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if text.is_empty() {
            return;
        }

        self.surface.write_text(text);

        self.invalidate(cx);
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let cursor = self.frame_cache.current()?.cursor()?;
        let cell = self.cell_metrics?;

        // `element_bounds` is the terminal leaf's content rect (padding already
        // excluded), so the cursor cell offsets from its origin directly — plus
        // the inter-block gap offset for the cursor's row.
        let offsets = self.current_row_offsets(cx);

        let mut y_offset = row_y_offset(&offsets, cursor.row as usize);

        // Block list: the live grid starts at `active_top` in the list.
        if self.block_list_mode(cx) {
            y_offset += self.frozen_hit.active_top;
        }

        Some(Bounds::new(
            point(
                element_bounds.left() + px(cursor.col as f32 * cell.width_px),
                element_bounds.top() + px(cursor.row as f32 * cell.height_px + y_offset),
            ),
            size(px(cell.width_px), px(cell.height_px)),
        ))
    }

    // No editable document and no preedit: text and marked-text methods are inert.
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        // GPUI's Windows IME path queries bounds only after obtaining a
        // selection; an empty virtual caret keeps commit-only input eligible.
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        _new_text: &str,
        _new_selected: Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.dirty.begin_frame();

        self.wake.mark_delivered(self.id);

        // Host events are drained by the shell pump (observer), and the surface
        // is resized from the leaf's actual bounds in paint — neither happens
        // here, so background tabs and chrome offsets are handled correctly.
        let cell = self.cell_metrics(window, cx);

        if self.frame_cache.needs_rebuild() {
            self.refresh_frame();
        }

        let frame = self.frame_cache.current().unwrap_or_default();

        // Release the atlas tiles of Kitty images whose final reference dropped
        // (replaced, removed, evicted, or last frozen owner gone). Gated
        // on the lock-free live-image check so a graphics-free session never touches
        // the store here.
        if self.surface.has_live_images() {
            for image in self.surface.drain_released_images() {
                let _ = window.drop_image(image);
            }
        }

        let settings = cx.global::<AppSettings>();
        let fixed_bottom = settings.input_style.is_fixed_bottom();
        let show_block_chrome = settings.command_blocks;

        let block_list_mode = self.block_list_mode(cx);

        // The tick only repaints the running header's elapsed time; compact
        // presentation hides headers, so skip it there.
        if block_list_mode && show_block_chrome && self.in_flight.is_some() {
            self.schedule_block_tick(cx);
        }

        // Block-split list: native GPUI list owns visibility, clamp, resize
        // anchoring, and tail following.
        let viewport_px = self
            .content_bounds
            .map(|b| b.size.height.as_f32())
            .unwrap_or(0.0);

        let block_list_element = if block_list_mode {
            let cols = self.content_cols();
            let pad_rows = block_pad_rows(cx);
            let store = self.surface.block_store();
            let live_rows = frame_content_rows(&frame);
            let history_rows = self.live_history_rows(&frame);
            let metrics = {
                let store = store.lock();
                block_list_render_metrics(
                    &store,
                    live_rows,
                    history_rows,
                    cols,
                    cell.height_px,
                    pad_rows,
                    self.block_list.list.logical_scroll_top(),
                )
            };

            let store_len = metrics.store_len;
            let evicted_items = metrics.evicted_items;
            let item_count = metrics.item_count;
            let evicted_delta =
                evicted_items.saturating_sub(self.block_list.evicted_items) as usize;

            self.selected_frozen_item = shift_selected_item_for_eviction(
                self.selected_frozen_item,
                evicted_delta,
                store_len,
            );

            match plan_list_reconcile(self.block_list.item_count, evicted_delta, item_count) {
                ListReconcile::Reset => self.block_list.list.reset(item_count),
                ListReconcile::Patch {
                    front_evict,
                    tail_splice,
                } => {
                    if front_evict > 0 {
                        self.block_list.list.splice(0..front_evict, 0);
                    }

                    if let Some((range, count)) = tail_splice {
                        self.block_list.list.splice(range, count);
                    }
                }
            }

            self.block_list.item_count = item_count;
            self.block_list.evicted_items = evicted_items;

            let measure_key = BlockListMeasureKey {
                layout: (cols, cell.height_px, pad_rows),
                store_len,
                evicted_items,
                last_item_px: metrics.last_item_px,
                tail_px: metrics.tail_px,
                live_rows,
            };

            match plan_remeasure(self.last_list_measure_key, measure_key) {
                RemeasureScope::All => self.block_list.list.remeasure(),
                RemeasureScope::Tail => {
                    let start = store_len.saturating_sub(1);
                    self.block_list.list.remeasure_items(start..item_count);
                }
                RemeasureScope::None => {}
            }

            self.last_list_measure_key = Some(measure_key);

            let pane = cx.entity();

            if !self.block_list.scroll_handler_set {
                self.block_list.list.set_scroll_handler({
                    let pane = pane.clone();
                    move |_, _window, cx| {
                        let _ = pane.update(cx, |pane, cx| pane.mark_scroll_activity(cx));
                    }
                });

                self.block_list.scroll_handler_set = true;
            }

            let total_px = metrics.total_px;
            let max_scroll = (total_px - viewport_px).max(0.0);
            let offset_px = metrics.offset_px.min(max_scroll);

            self.block_list.scrollbar = (offset_px, max_scroll);
            self.block_list.active_top = block_list_active_top_px(
                metrics.frozen_px,
                metrics.tail_px,
                cell.height_px,
                pad_rows,
                offset_px,
            );

            let frame_for_items = frame.clone();
            let in_flight_for_items = self.in_flight.clone();
            let has_open_prompt_for_items = self.open_prompt;
            let selected_frozen_item = self.selected_frozen_item;
            let frozen_selection = self.frozen_selection;
            let cell_for_items = cell;
            let pane_for_items = pane.clone();
            let store_for_items = store.clone();
            let live_index = item_count.saturating_sub(1);

            Some(
                list(self.block_list.list.clone(), move |ix, _window, _cx| {
                    if ix < live_index {
                        BlockListItem::Frozen {
                            item_idx: ix,
                            store: store_for_items.clone(),
                            cols,
                            cell: cell_for_items,
                            selection: frozen_selection,
                            selected_item: selected_frozen_item,
                            pane: pane_for_items.clone(),
                        }
                        .into_any_element()
                    } else {
                        BlockListItem::Live {
                            frame: frame_for_items.clone(),
                            history_rows,
                            in_flight: in_flight_for_items.clone(),
                            has_open_prompt: has_open_prompt_for_items,
                            live_index,
                            selected_item: selected_frozen_item,
                            cols,
                            cell: cell_for_items,
                            pane: pane_for_items.clone(),
                        }
                        .into_any_element()
                    }
                })
                .size_full()
                .into_any_element(),
            )
        } else {
            None
        };

        // Auto-hide: the scrollbar stays solid briefly, then fades out.
        let scrollbar_opacity = scrollbar_opacity(
            self.scrollbar_dragging,
            self.last_scroll_activity.map(|at| at.elapsed()),
        );

        if scrollbar_opacity.is_some_and(|opacity| opacity < 1.0) {
            window.request_animation_frame();
        }

        let scrollbar_info = if block_list_element.is_some() {
            ScrollbarInfo {
                total: (self.block_list.scrollbar.1 + viewport_px).max(0.0) as u64,
                offset: self.block_list.scrollbar.0.max(0.0) as u64,
                len: viewport_px.max(0.0) as u64,
            }
        } else {
            frame.scrollbar()
        };

        // Keep the transparent track hit-testable so hovering the scrollbar
        // region can reveal it after the activity fade has completed.
        let scrollbar = scrollbar_element(scrollbar_info, scrollbar_opacity.unwrap_or(0.0), cx);

        div()
            // Stateful id: hover-end tracking (the link-underline clear
            // below) needs element state.
            .id(("terminal-pane", self.id as usize))
            .size_full()
            .relative()
            // This is the terminal region's single full-bleed background;
            // cells with explicit background colors stay opaque on top.
            .bg(rgb(theme_default_background().rgb_u32()).opacity(surface_background_opacity(cx)))
            // The shell frames each pane as a 1px-bordered rounded card; the
            // fill is rounded to the card's inner radius so its corners don't
            // paint square over the frame. The cell padding below keeps glyphs
            // clear of the rounded corners.
            .rounded(cx.theme().radius_lg - px(1.))
            .text_color(rgb(theme_default_foreground().rgb_u32()))
            .font_family(metrics::font_family(cx))
            .text_size(px(metrics::font_size_px(cx)))
            .line_height(px(cell.height_px))
            .p(px(metrics::PADDING_PX))
            .overflow_hidden()
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_action(cx.listener(Self::on_send_tab))
            .on_action(cx.listener(Self::on_send_shift_tab))
            .on_action(cx.listener(Self::on_copy_block_command))
            .on_action(cx.listener(Self::on_copy_block_output))
            .on_action(cx.listener(Self::on_rerun_block))
            .on_action(cx.listener(Self::on_previous_block))
            .on_action(cx.listener(Self::on_next_block))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_modifiers_changed(cx.listener(Self::on_modifiers_changed))
            .on_drop(cx.listener(Self::on_file_drop))
            // Moving off the pane produces no further mouse-move events here,
            // so hover end is what clears a still-Ctrl-held underline.
            .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                if !hovered {
                    this.last_mouse_position = None;
                    if this.hovered_link.take().is_some() {
                        cx.notify();
                    }
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .child(if let Some(list_element) = block_list_element {
                BlockListView {
                    cell,
                    focus: self.focus.clone(),
                    pane: cx.entity(),
                    list: list_element,
                    show_chrome: show_block_chrome,
                }
                .into_any_element()
            } else {
                TerminalView::new(frame, cell, self.focus.clone(), cx.entity(), fixed_bottom)
                    .into_any_element()
            })
            .children(scrollbar)
            // Ctrl-hover link underline. Rects are content-origin-relative;
            // absolute children position from the padding box, so shift by
            // the content padding.
            .when_some(self.hovered_link.as_ref(), |this, link| {
                this.cursor_pointer()
                    .children(link.rects.iter().map(|rect| {
                        div()
                            .absolute()
                            .left(rect.origin.x + px(metrics::PADDING_PX))
                            .top(rect.origin.y + px(metrics::PADDING_PX))
                            .w(rect.size.width)
                            .h(rect.size.height)
                            .bg(rgb(theme_default_foreground().rgb_u32()))
                    }))
            })
    }
}

fn selection_drag_started(origin: Point<Pixels>, position: Point<Pixels>, cell_width: f32) -> bool {
    let dx = position.x.as_f32() - origin.x.as_f32();
    let dy = position.y.as_f32() - origin.y.as_f32();

    dx * dx + dy * dy >= cell_width * cell_width / 16.0
}

fn selection_type_for_click_count(click_count: usize) -> SelectionType {
    match click_count {
        2 => SelectionType::Semantic,
        3.. => SelectionType::Lines,
        _ => SelectionType::Simple,
    }
}

/// Map a pointer position to a grid cell. `offsets` shifts rows for bottom anchoring.
pub(super) fn terminal_cell_at_position(
    position: Point<Pixels>,
    origin: Point<Pixels>,
    cell: metrics::CellMetrics,
    offsets: &[f32],
) -> (SurfaceCell, SurfaceCellSide) {
    let x = (position.x.as_f32() - origin.x.as_f32()).max(0.0);
    let y = (position.y.as_f32() - origin.y.as_f32()).max(0.0);
    let col = (x / cell.width_px).floor() as u16;
    let row = terminal_row_at_y(y, cell.height_px, offsets);
    let cell_x = x - (col as f32 * cell.width_px);

    let side = if cell_x < cell.width_px / 2.0 {
        SurfaceCellSide::Left
    } else {
        SurfaceCellSide::Right
    };

    (SurfaceCell { col, row }, side)
}

fn surface_mouse_button(button: MouseButton) -> Option<SurfaceMouseButton> {
    match button {
        MouseButton::Left => Some(SurfaceMouseButton::Left),
        MouseButton::Middle => Some(SurfaceMouseButton::Middle),
        MouseButton::Right => Some(SurfaceMouseButton::Right),
        MouseButton::Navigate(_) => None,
    }
}

const WHEEL_LINES_PER_STEP: f32 = 3.0;

fn terminal_scroll_lines(delta: ScrollDelta, cell: metrics::CellMetrics) -> i32 {
    let raw = match delta {
        ScrollDelta::Lines(point) => point.y * WHEEL_LINES_PER_STEP,
        ScrollDelta::Pixels(point) => point.y.as_f32() / cell.height_px.max(1.0),
    };

    if raw.abs() < 0.5 {
        0
    } else {
        raw.round() as i32
    }
}

fn should_scroll_to_latest(keystroke: &Keystroke, alt_screen: bool) -> bool {
    !alt_screen && !keystroke.modifiers.modified() && keystroke.key.eq_ignore_ascii_case("end")
}

fn dropped_paths_text(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| {
            let path = path.to_string_lossy();
            if path.contains(' ') {
                format!("\"{path}\"")
            } else {
                path.into_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use gpui::{ScrollDelta, point, px};
    use nmt_terminal::selection::SelectionType;

    use super::{
        dropped_paths_text, metrics, selection_drag_started, selection_type_for_click_count,
        terminal_cell_at_position, terminal_scroll_lines,
    };
    use crate::terminal::surface::{SurfaceCell, SurfaceCellSide};

    #[test]
    fn repeated_clicks_choose_terminal_selection_modes() {
        assert_eq!(selection_type_for_click_count(1), SelectionType::Simple);
        assert_eq!(selection_type_for_click_count(2), SelectionType::Semantic);
        assert_eq!(selection_type_for_click_count(3), SelectionType::Lines);
        assert_eq!(selection_type_for_click_count(4), SelectionType::Lines);
    }

    #[test]
    fn dropped_paths_are_space_delimited_and_paths_with_spaces_are_quoted() {
        assert_eq!(
            dropped_paths_text(&[
                "C:\\src\\main.rs".into(),
                "C:\\My Project\\notes.txt".into(),
            ]),
            "C:\\src\\main.rs \"C:\\My Project\\notes.txt\""
        );
    }

    #[test]
    fn block_gutter_hit_band() {
        use super::block_gutter_hit;
        let origin_x = 10.0;
        assert!(block_gutter_hit(10.0 - 5.0, origin_x), "on the strip");
        assert!(
            block_gutter_hit(10.0 + 2.0, origin_x),
            "tolerance into col 0"
        );
        assert!(
            !block_gutter_hit(10.0 + 6.0, origin_x),
            "column 0 text is not the gutter"
        );
        assert!(
            block_gutter_hit(0.0, origin_x),
            "strip is flush with the pane edge"
        );
        assert!(!block_gutter_hit(-3.0, origin_x), "left of the pane misses");
    }

    #[test]
    fn mouse_position_maps_to_cell_and_side() {
        let cell = metrics::CellMetrics {
            width_px: 8.0,
            height_px: 18.0,
        };

        // Content origin at (10, 10): position 26,46 -> local 16,36 -> col 2 row 2.
        let origin = point(px(10.0), px(10.0));
        assert_eq!(
            terminal_cell_at_position(point(px(26.0), px(46.0)), origin, cell, &[]),
            (SurfaceCell { col: 2, row: 2 }, SurfaceCellSide::Left)
        );
        assert_eq!(
            terminal_cell_at_position(point(px(31.0), px(46.0)), origin, cell, &[]),
            (SurfaceCell { col: 2, row: 2 }, SurfaceCellSide::Right)
        );
    }

    #[test]
    fn selection_drag_waits_for_quarter_cell_movement() {
        let origin = point(px(10.0), px(10.0));

        assert!(!selection_drag_started(
            origin,
            point(px(11.0), px(11.0)),
            8.0
        ));
        assert!(selection_drag_started(
            origin,
            point(px(12.0), px(10.0)),
            8.0
        ));
    }

    #[test]
    fn scroll_delta_maps_to_terminal_lines() {
        let cell = metrics::CellMetrics {
            width_px: 8.0,
            height_px: 20.0,
        };

        assert_eq!(
            terminal_scroll_lines(ScrollDelta::Pixels(point(px(0.0), px(60.0))), cell),
            3
        );
        assert_eq!(
            terminal_scroll_lines(ScrollDelta::Lines(point(0.0, -2.0)), cell),
            -6
        );
        assert_eq!(
            terminal_scroll_lines(ScrollDelta::Pixels(point(px(0.0), px(4.0))), cell),
            0
        );
    }
}
