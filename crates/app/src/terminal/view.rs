use std::ops::Range;

use futures::StreamExt;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, AppContext, AvailableSpace, Bounds, ContentMask, Context, Corners,
    DragMoveEvent, Element, ElementId, ElementInputHandler, Entity, EntityInputHandler,
    FocusHandle, Focusable, FontStyle, FontWeight, GlobalElementId, InspectorElementId,
    IntoElement, KeyDownEvent, Keystroke, LayoutId, ListAlignment, ListOffset, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, RenderImage, ScrollDelta,
    ScrollWheelEvent, ShapedLine, StrikethroughStyle, Style, TextAlign, TextRun, UTF16Selection,
    UnderlineStyle, Window, actions, div, fill, list, point, px, relative, rgb, rgba, size,
};
use nmt_agent_hook::{AgentRoute, agent_process};
use nmt_config::local_state::TabState;
use nmt_terminal::ansi::CursorShape;
use nmt_terminal::selection::SelectionType;

use super::frame::{
    TerminalColor, TerminalCursor, TerminalFrame, TerminalFrameCache, TerminalLine,
    theme_default_background, theme_default_foreground,
};
use super::surface::TerminalSurface;
use super::{input, metrics, wake};
use crate::terminal::block_list::{BlockListPoint, BlockListState, FrozenPoint};
use crate::terminal::dirty::DirtyState;
use crate::terminal::session::{HostEvent, InFlightBlock};
use crate::terminal::surface::{
    SurfaceCell, SurfaceCellSide, SurfaceMouseButton, SurfaceMouseEventKind, SurfaceScreenCell,
    TerminalKeyAction as SurfaceKeyAction,
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

pub(crate) struct TerminalPane {
    pub(crate) focus: FocusHandle,
    /// Surface/tab id (same value as this pane's `TabId`); the shell pump uses it
    /// to route host events to the owning tab.
    id: u64,
    agent_route: AgentRoute,
    surface: TerminalSurface,
    frame_cache: TerminalFrameCache,
    cell_metrics: Option<metrics::CellMetrics>,
    /// The terminal leaf's laid-out content rect (window coords, padding
    /// excluded), set from the element's paint. Resize and pointer hit-testing use
    /// it so chrome (tab bar) offsets are honored instead of assuming the window.
    content_bounds: Option<Bounds<Pixels>>,
    /// True while the scrollbar thumb is being dragged (mouse-move then scrolls
    /// to the pointer instead of selecting text).
    scrollbar_dragging: bool,
    /// Last user scroll action; the scrollbar shows only while dragging or
    /// within [`SCROLLBAR_LINGER`] of this instant, then auto-hides.
    last_scroll_activity: Option<std::time::Instant>,
    /// Bumped per scroll action so only the newest hide-timer repaints.
    scroll_activity_gen: u64,
    /// Pointer offset inside the thumb at drag start (track fraction), so
    /// grabbing the thumb doesn't jump it.
    scrollbar_grab: f32,
    wake: wake::WakeSignal,
    dirty: DirtyState,
    /// The in-flight command mirrored from the session on drain.
    in_flight: Option<InFlightBlock>,
    /// Whether a trusted prompt input region is open.
    open_prompt: bool,
    block_list: BlockListState,
    /// Hit-test data recorded from the last native list prepaint.
    frozen_hit: crate::terminal::block_list::FrozenHitInfo,
    /// Inputs that affect measured heights of the mutable tail of the native list.
    last_list_measure_key: Option<BlockListMeasureKey>,
    /// The gutter-selected frozen item (block-split): highlighted and
    /// targeted by the copy/re-run/jump actions in list mode.
    selected_frozen_item: Option<usize>,
    /// Visible frozen item chrome recorded from native list item bounds.
    frozen_chrome: Vec<crate::terminal::block_list::FrozenItemChrome>,
    /// Frozen-region selection: (anchor, head), both inclusive cell points.
    frozen_selection: Option<(
        crate::terminal::block_list::FrozenPoint,
        crate::terminal::block_list::FrozenPoint,
    )>,
    /// Visible separator y positions, painted outside GPUI List's content mask.
    frozen_separators: Vec<f32>,
    /// Anchor of an in-progress frozen-region drag. The selection itself is
    /// only created on the first mouse-move, so a plain click selects nothing
    /// (matching the engine's empty-selection-dropped-on-up semantics).
    frozen_select_anchor: Option<crate::terminal::block_list::FrozenPoint>,
    /// Pixel origin of a text-selection gesture. Ignoring movement within a
    /// quarter-cell radius prevents normal hand jitter from selecting a glyph.
    selection_drag_origin: Option<Point<Pixels>>,
}

pub(crate) struct AgentInterrupted;

impl gpui::EventEmitter<AgentInterrupted> for TerminalPane {}

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
        let (wake, wake_rx) = wake::wake_channel();
        let agent_route = agent_process().allocate_route();
        let environment = agent_process().environment_for(&agent_route);
        let fixed_bottom_requested =
            cx.read_global(|settings: &AppSettings, _| settings.input_style.is_fixed_bottom());
        let surface = terminal_surface_for_tab(
            &wake,
            surface_id,
            &tab_state,
            fixed_bottom_requested,
            environment,
        )?;
        Ok(cx.new(|cx| {
            Self::from_surface(
                cx,
                surface_id,
                agent_route,
                wake,
                wake_rx,
                surface,
                fixed_bottom_requested,
            )
        }))
    }

    fn from_surface(
        cx: &mut Context<Self>,
        surface_id: u64,
        agent_route: AgentRoute,
        wake: wake::WakeSignal,
        mut wake_rx: wake::WakeReceiver,
        surface: TerminalSurface,
        fixed_bottom_requested: bool,
    ) -> Self {
        // Apply terminal presentation settings to existing panes and invalidate
        // measurements that depend on font metrics.
        cx.observe_global::<AppSettings>(|this, cx| {
            let fixed_bottom = cx.global::<AppSettings>().input_style.is_fixed_bottom();
            this.block_list
                .list
                .set_alignment(block_list_alignment(fixed_bottom));
            this.surface.set_theme_colors(&nmt_config::active_colors());
            this.cell_metrics = None;
            this.frame_cache.invalidate();
            cx.notify();
        })
        .detach();

        cx.spawn(async move |this, cx| {
            while wake_rx.next().await.is_some() {
                if this
                    .update(cx, |this, cx| {
                        this.invalidate(cx);
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
            agent_route,
            surface,
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
        }
    }

    pub(crate) fn agent_route(&self) -> &AgentRoute {
        &self.agent_route
    }

    /// Record a user scroll action and schedule the repaint that starts fading
    /// the scrollbar once [`SCROLLBAR_LINGER`] passes without further activity.
    fn mark_scroll_activity(&mut self, cx: &mut Context<Self>) {
        self.last_scroll_activity = Some(std::time::Instant::now());
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
    fn content_origin(&self) -> Point<Pixels> {
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
        self.frame_cache.rebuild(self.surface.frame());
    }

    fn invalidate(&mut self, cx: &mut Context<Self>) {
        self.frame_cache.invalidate();
        if self.dirty.mark() {
            cx.notify();
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
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
                self.frozen_selection = None;
                cx.notify();
                return;
            }
        }
        if self.surface.apply_key_action(action) {
            if interrupts_agent {
                cx.emit(AgentInterrupted);
            }
            self.invalidate(cx);
        }
    }

    /// Route a keystroke straight to the terminal PTY.
    pub(crate) fn feed_terminal_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        if self.surface.apply_key_action(input::key_action(keystroke)) {
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
                modifiers: gpui::Modifiers::none(),
                key: "tab".into(),
                key_char: None,
            },
            cx,
        );
    }

    fn on_send_shift_tab(&mut self, _: &SendShiftTab, _: &mut Window, cx: &mut Context<Self>) {
        self.feed_terminal_key(
            &Keystroke {
                modifiers: gpui::Modifiers::shift(),
                key: "tab".into(),
                key_char: None,
            },
            cx,
        );
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        self.selection_drag_origin = None;
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
    fn scrollbar_fraction(&self, y: Pixels) -> f32 {
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
    fn scroll_thumb_to(&mut self, thumb_top: f32, cx: &mut Context<Self>) {
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

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            // Clamp into the frozen region so a drag past the seam sticks to
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
        modifiers: gpui::Modifiers,
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
    fn current_row_offsets(&self, cx: &App) -> Vec<f32> {
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

    /// The pane's last laid-out content size (`None` before the first paint).
    /// Split creation uses it to check the focused pane can yield the minimum
    /// panel size.
    pub(crate) fn content_size(&self) -> Option<gpui::Size<Pixels>> {
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
                    tracing::info!(interactive = *on, "terminal interactive state changed");
                }
                HostEvent::AltScreen(on) => {
                    self.surface.set_alt_screen(*on);
                }
                HostEvent::PromptBoundaryTrusted(on) => {
                    tracing::info!(
                        prompt_boundary_trusted = *on,
                        "terminal prompt boundary trust changed"
                    );
                }
                HostEvent::Cwd(cwd) => self.surface.set_last_cwd(cwd.clone()),
                HostEvent::Title(_)
                | HostEvent::Bell
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
            // Trust loss / exit clear the session's in-flight block; mirror it.
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
    fn block_list_mode(&self, _cx: &App) -> bool {
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
        store: &nmt_terminal::block_store::BlockStore,
        frame: &TerminalFrame,
        cols: u32,
        cell_h: f32,
        pad_rows: f32,
    ) -> f32 {
        let frozen: f32 = store
            .items()
            .iter()
            .map(|item| crate::terminal::block_list::item_px(item, cols, cell_h, pad_rows))
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
        store: &nmt_terminal::block_store::BlockStore,
        frame: &TerminalFrame,
        cols: u32,
        cell_h: f32,
        pad_rows: f32,
        target: f32,
    ) -> ListOffset {
        let mut y = 0.0f32;
        for (ix, item) in store.items().iter().enumerate() {
            let h = crate::terminal::block_list::item_px(item, cols, cell_h, pad_rows);
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

    fn begin_block_list_frame(
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

    fn record_frozen_view(
        &mut self,
        view: &crate::terminal::block_list::FrozenView,
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

    fn record_frozen_chrome(
        &mut self,
        chrome: crate::terminal::block_list::FrozenItemChrome,
        item_top: f32,
    ) {
        self.frozen_chrome
            .push(offset_frozen_chrome(chrome, item_top));
    }

    /// Map a window position to either an immutable block row or an absolute
    /// SCREEN row from the active block's history.
    fn block_list_point_at(&self, position: Point<Pixels>, cx: &App) -> Option<BlockListPoint> {
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
        let target = {
            let store = store.lock();
            crate::terminal::block_list::nav_item_top(
                &store,
                cols,
                cell.height_px,
                pad_rows,
                resolved,
                direction,
            )
        };
        let Some(target) = target else {
            return;
        };
        let store = self.surface.block_store();
        let store = store.lock();
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
                .timer(std::time::Duration::from_secs(1))
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
        a: crate::terminal::block_list::FrozenPoint,
        b: crate::terminal::block_list::FrozenPoint,
    ) -> String {
        let pieces = {
            let store = self.surface.block_store();
            let store = store.lock();
            crate::terminal::block_list::frozen_selection_pieces(&store, a, b)
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
        handle: nmt_terminal::ghostty::BlockHandle,
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
    fixed_bottom_requested: bool,
    environment_overrides: Vec<(String, String)>,
) -> Result<TerminalSurface, String> {
    match TerminalSurface::for_gpui(
        wake.clone(),
        surface_id,
        state.shell.clone(),
        state.args.clone(),
        state.cwd.clone(),
        fixed_bottom_requested,
        environment_overrides.clone(),
    ) {
        Ok(surface) => Ok(surface),
        Err(error) if state.cwd.is_some() => {
            tracing::warn!("restored tab failed with saved cwd, retrying without cwd: {error}");
            TerminalSurface::for_gpui(
                wake.clone(),
                surface_id,
                state.shell.clone(),
                state.args.clone(),
                None,
                fixed_bottom_requested,
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
        self.wake.mark_delivered();

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
            let mut mirrored_count = self.block_list.item_count;
            if evicted_delta > 0 {
                let old_frozen = mirrored_count.saturating_sub(1);
                if evicted_delta > old_frozen {
                    self.block_list.list.reset(item_count);
                    mirrored_count = item_count;
                } else {
                    self.block_list.list.splice(0..evicted_delta, 0);
                    mirrored_count -= evicted_delta;
                }
            }
            if item_count < mirrored_count {
                self.block_list.list.reset(item_count);
            } else if item_count != mirrored_count {
                let old_live = mirrored_count.saturating_sub(1);
                self.block_list
                    .list
                    .splice(old_live..mirrored_count, item_count - old_live);
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
            if self
                .last_list_measure_key
                .is_some_and(|prev| prev.layout != measure_key.layout)
            {
                self.block_list.list.remeasure();
            } else if self.last_list_measure_key != Some(measure_key) {
                let start = store_len.saturating_sub(1);
                self.block_list.list.remeasure_items(start..item_count);
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
            nmt_terminal::ghostty::ScrollbarInfo {
                total: (self.block_list.scrollbar.1 + viewport_px).max(0.0) as u64,
                offset: self.block_list.scrollbar.0.max(0.0) as u64,
                len: viewport_px.max(0.0) as u64,
            }
        } else {
            frame.scrollbar()
        };
        let scrollbar =
            scrollbar_opacity.and_then(|opacity| scrollbar_element(scrollbar_info, opacity, cx));

        div()
            .size_full()
            .relative()
            // This is the terminal region's single full-bleed background;
            // cells with explicit background colors stay opaque on top.
            .bg(rgb(theme_default_background().rgb_u32()).opacity(surface_background_opacity(cx)))
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
    }
}

/// A right-edge scrollbar overlay, shown only when there is scrollback. The thumb
/// size/position reflect the viewport within the total, and clicking or dragging
/// the track scrolls to that offset.
struct ScrollbarDrag;

fn scrollbar_thumb_geometry(total: f64, offset: f64, len: f64) -> Option<(f32, f32)> {
    if total <= len {
        return None;
    }
    let thumb_height = (len / total).clamp(0.03, 1.0) as f32;
    let scrollable = total - len;
    let thumb_top = (offset.clamp(0.0, scrollable) / scrollable) as f32 * (1.0 - thumb_height);
    Some((thumb_top, thumb_height))
}

fn scrollbar_offset_for_thumb(total: f64, len: f64, thumb_top: f32) -> Option<f64> {
    let (_, thumb_height) = scrollbar_thumb_geometry(total, 0.0, len)?;
    let thumb_travel = 1.0 - thumb_height;
    Some((thumb_top / thumb_travel).clamp(0.0, 1.0) as f64 * (total - len))
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

fn scrollbar_element(
    sb: nmt_terminal::ghostty::ScrollbarInfo,
    opacity: f32,
    cx: &mut Context<TerminalPane>,
) -> Option<gpui::Stateful<gpui::Div>> {
    let (thumb_top, thumb_height) =
        scrollbar_thumb_geometry(sb.total as f64, sb.offset as f64, sb.len as f64)?;
    Some(
        div()
            .id("terminal-scrollbar")
            .absolute()
            .top_0()
            .right_0()
            .h_full()
            .w(px(10.0))
            .opacity(opacity)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                    this.scrollbar_dragging = true;
                    let fraction = this.scrollbar_fraction(event.position.y);
                    if (thumb_top..thumb_top + thumb_height).contains(&fraction) {
                        // Grab the thumb where the pointer hit it — no jump.
                        this.scrollbar_grab = fraction - thumb_top;
                    } else {
                        // Track click: center the thumb on the pointer.
                        this.scrollbar_grab = thumb_height / 2.0;
                        this.scroll_thumb_to(fraction - this.scrollbar_grab, cx);
                    }
                    this.mark_scroll_activity(cx);
                }),
            )
            .on_drag(ScrollbarDrag, |_, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| gpui::Empty)
            })
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ScrollbarDrag>, window, cx| {
                    if this.scrollbar_dragging {
                        cx.stop_propagation();
                        this.on_mouse_move(&event.event, window, cx);
                    }
                }),
            )
            .child(
                div()
                    .absolute()
                    .top(relative(thumb_top))
                    .h(relative(thumb_height))
                    .w_full()
                    .rounded(px(4.0))
                    .bg(rgba(0xffffff40)),
            ),
    )
}

/// Map a pointer position to a grid cell. `offsets` shifts rows for bottom anchoring.
fn terminal_cell_at_position(
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

/// How long the scrollbar stays visible after the last scroll action.
const SCROLLBAR_LINGER: std::time::Duration = std::time::Duration::from_millis(900);
/// How long the scrollbar takes to fade out after lingering.
const SCROLLBAR_FADE: std::time::Duration = std::time::Duration::from_millis(180);

fn scrollbar_opacity(
    dragging: bool,
    elapsed_since_scroll: Option<std::time::Duration>,
) -> Option<f32> {
    if dragging {
        return Some(1.0);
    }
    let elapsed = elapsed_since_scroll?;
    if elapsed < SCROLLBAR_LINGER {
        return Some(1.0);
    }
    if elapsed >= SCROLLBAR_LINGER + SCROLLBAR_FADE {
        return None;
    }
    let fade_elapsed = elapsed - SCROLLBAR_LINGER;
    Some(1.0 - fade_elapsed.as_secs_f32() / SCROLLBAR_FADE.as_secs_f32())
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

/// The terminal viewport as a custom GPUI leaf element: prepaint shapes the
/// visible rows (multi-run, per-cell foreground), paint draws backgrounds, the
/// styled glyphs, and the cursor. Mirrors GPUI's `Canvas` element shape.
pub(crate) struct TerminalView {
    frame: TerminalFrame,
    cell: metrics::CellMetrics,
    focus: FocusHandle,
    pane: Entity<TerminalPane>,
    /// FixedBottom input style: bottom-anchor the grid so the last content row
    /// pins to the viewport floor (Warp parity, incl. interactive output).
    fixed_bottom: bool,
}

impl TerminalView {
    pub(crate) fn new(
        frame: TerminalFrame,
        cell: metrics::CellMetrics,
        focus: FocusHandle,
        pane: Entity<TerminalPane>,
        fixed_bottom: bool,
    ) -> Self {
        Self {
            frame,
            cell,
            focus,
            pane,
            fixed_bottom,
        }
    }
}

impl IntoElement for TerminalView {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for TerminalView {
    type RequestLayoutState = Style;
    type PrepaintState = Vec<ShapedLine>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Style) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        let layout_id = window.request_layout(style.clone(), [], cx);
        (layout_id, style)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Style,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<ShapedLine> {
        // Feed the real content rect back to the pane so it resizes the surface to
        // its actual area (below the tab bar), not the full window.
        let cell = self.cell;
        self.pane
            .update(cx, |pane, cx| pane.set_content_bounds(bounds, cell, cx));
        shape_frame(bounds, &self.frame, self.cell, window)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Style,
        prepaint: &mut Vec<ShapedLine>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let offsets = bottom_anchor_offsets(&self.frame, self.cell.height_px, self.fixed_bottom);
        paint_frame(
            bounds,
            &self.frame,
            prepaint.as_slice(),
            self.cell,
            &offsets,
            window,
            cx,
        );
        // Register commit-only IME for the focused pane; self-gates on focus.
        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, self.pane.clone()),
            cx,
        );
    }
}

/// Block-split list wrapper: the child is a real `gpui::list`; this wrapper
/// only feeds pane bounds, paints chrome that extends into the left padding,
/// and keeps the IME handler attached to the full terminal content rect.
pub(crate) struct BlockListView {
    cell: metrics::CellMetrics,
    focus: FocusHandle,
    pane: Entity<TerminalPane>,
    list: AnyElement,
    show_chrome: bool,
}

impl IntoElement for BlockListView {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for BlockListView {
    type RequestLayoutState = Style;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Style) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        let layout_id = window.request_layout(style.clone(), [], cx);
        (layout_id, style)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Style,
        window: &mut Window,
        cx: &mut App,
    ) {
        let cell = self.cell;
        self.pane
            .update(cx, |pane, cx| pane.begin_block_list_frame(bounds, cell, cx));
        self.list.layout_as_root(
            size(
                AvailableSpace::Definite(bounds.size.width),
                AvailableSpace::Definite(bounds.size.height),
            ),
            window,
            cx,
        );
        self.list.prepaint_at(bounds.origin, window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Style,
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let pane = self.pane.read(cx);
        let separators = pane.frozen_separators.clone();
        let chrome = pane.frozen_chrome.clone();
        if self.show_chrome {
            crate::terminal::block_list::paint_frozen_separators(bounds, &separators, window);
        }
        self.list.paint(window, cx);
        if self.show_chrome {
            crate::terminal::block_list::paint_frozen_chrome(bounds, &chrome, window, cx);
        }
        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, self.pane.clone()),
            cx,
        );
    }
}

type SharedBlockStore = std::sync::Arc<parking_lot::Mutex<nmt_terminal::block_store::BlockStore>>;

enum BlockListItem {
    Frozen {
        item_idx: usize,
        store: SharedBlockStore,
        cols: u32,
        cell: metrics::CellMetrics,
        selection: Option<(
            crate::terminal::block_list::FrozenPoint,
            crate::terminal::block_list::FrozenPoint,
        )>,
        selected_item: Option<usize>,
        pane: Entity<TerminalPane>,
    },
    Live {
        frame: TerminalFrame,
        /// Active-grid scrollback rows rendered above the live grid
        /// when scrolling into a running command.
        history_rows: u64,
        in_flight: Option<InFlightBlock>,
        has_open_prompt: bool,
        live_index: usize,
        selected_item: Option<usize>,
        cols: u32,
        cell: metrics::CellMetrics,
        pane: Entity<TerminalPane>,
    },
}

impl IntoElement for BlockListItem {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

enum BlockListItemPrepaint {
    Frozen {
        view: crate::terminal::block_list::FrozenView,
        shaped: Vec<ShapedLine>,
    },
    Live {
        tail_view: crate::terminal::block_list::FrozenView,
        tail_shaped: Vec<ShapedLine>,
        active_shaped: Vec<ShapedLine>,
    },
}

impl Element for BlockListItem {
    type RequestLayoutState = Style;
    type PrepaintState = BlockListItemPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Style) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        let pad_rows = block_pad_rows(cx);
        let height = match self {
            BlockListItem::Frozen {
                item_idx,
                store,
                cols,
                cell,
                ..
            } => {
                let store = store.lock();
                store
                    .items()
                    .get(*item_idx)
                    .map(|item| {
                        crate::terminal::block_list::item_px(item, *cols, cell.height_px, pad_rows)
                    })
                    .unwrap_or(0.0)
            }
            BlockListItem::Live {
                frame,
                history_rows,
                cell,
                ..
            } => crate::terminal::block_list::live_item_px(
                *history_rows,
                frame_content_rows(frame),
                cell.height_px,
                pad_rows,
            ),
        }
        .max(0.0);
        style.size.height = px(height).into();
        let layout_id = window.request_layout(style.clone(), [], cx);
        (layout_id, style)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Style,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let origin_y = self.pane().read(cx).content_origin().y;
        let item_top = (bounds.top() - origin_y).as_f32();
        let pad_rows = block_pad_rows(cx);
        match self {
            BlockListItem::Frozen {
                item_idx,
                store,
                cols: _,
                cell,
                selection,
                selected_item,
                pane,
            } => {
                // Snapshot the item under the store lock, then release it
                // before touching the engine — the PTY thread nests engine →
                // store, so the reverse nesting here would deadlock.
                let handle_info = {
                    let store = store.lock();
                    store
                        .items()
                        .get(*item_idx)
                        .and_then(crate::terminal::block_list::handle_item_info)
                };
                let mut view = match handle_info {
                    Some(info) => {
                        let visible = crate::terminal::block_list::visible_rows(
                            bounds.top().as_f32(),
                            info.rows,
                            window.viewport_size().height.as_f32(),
                            cell.height_px,
                            pad_rows,
                        );
                        let acquired = pane.read(cx).surface.acquire_block(info.handle);
                        let mut view = crate::terminal::block_list::frozen_block_view(
                            acquired.as_ref().map(|acq| (&acq.block, &acq.palette)),
                            &info,
                            *item_idx,
                            visible.clone(),
                            cell.height_px,
                            pad_rows,
                            *selection,
                            *selected_item,
                        );
                        // Resolve each frozen Kitty placement's
                        // generation from the session's (block_id, image_id)
                        // cache; misses read pixels out of the acquired block
                        // once and land in the cache for later frames.
                        if let Some(acq) = &acquired
                            && !acq.placements.is_empty()
                        {
                            let ids: std::collections::HashSet<u32> =
                                acq.placements.iter().map(|p| p.image_id).collect();
                            let surface = &pane.read(cx).surface;
                            let generations: std::collections::HashMap<_, _> = ids
                                .into_iter()
                                .filter_map(|id| {
                                    surface
                                        .frozen_image(info.handle.id, id)
                                        .or_else(|| {
                                            let generation =
                                                surface.frozen_image_generation(&acq.block, id)?;
                                            surface.insert_frozen_image(
                                                info.handle.id,
                                                id,
                                                generation.clone(),
                                            );
                                            Some(generation)
                                        })
                                        .map(|generation| (id, generation))
                                })
                                .collect();
                            view.images = crate::terminal::block_list::frozen_block_images(
                                &acq.placements,
                                &generations,
                                &visible,
                                cell.height_px,
                                pad_rows,
                            );
                        }
                        view
                    }
                    None => Default::default(),
                };
                pane.update(cx, |pane, _| pane.record_frozen_view(&view, item_top));
                view.items_chrome.clear();
                let shaped = crate::terminal::block_list::shape_frozen_rows(
                    &view.rows,
                    cell.width_px,
                    window,
                );
                BlockListItemPrepaint::Frozen { view, shaped }
            }
            BlockListItem::Live {
                frame,
                history_rows,
                in_flight,
                has_open_prompt,
                live_index,
                selected_item,
                cols,
                cell,
                pane,
            } => {
                // The active grid's scrollback rows render above the live
                // grid, visible range only; a running command's
                // scroll-up history).
                let tail_view = {
                    let visible = crate::terminal::block_list::visible_rows(
                        bounds.top().as_f32(),
                        (*history_rows).min(usize::MAX as u64) as usize,
                        window.viewport_size().height.as_f32(),
                        cell.height_px,
                        pad_rows,
                    );
                    let pane = pane.read(cx);
                    let lines = pane
                        .surface
                        .live_history_lines(visible.start as u64..visible.end as u64);
                    let selection = pane.surface.selection_screen_range();
                    crate::terminal::block_list::live_history_view(
                        lines,
                        *history_rows,
                        *cols,
                        cell.height_px,
                        pad_rows,
                        selection,
                    )
                };
                let live_rows = frame_content_rows(frame);
                let live_chrome = block_list_live_chrome(
                    *live_index,
                    live_rows,
                    cell.height_px,
                    in_flight.as_ref(),
                    *has_open_prompt,
                    *selected_item == Some(*live_index),
                );
                pane.update(cx, |pane, _| {
                    pane.record_frozen_view(&tail_view, item_top);
                    let active_top = item_top + tail_view.active_top;
                    pane.frozen_hit.set_active_top(active_top);
                    if let Some(mut chrome) = live_chrome {
                        chrome.bottom = tail_view.active_top
                            + live_rows as f32 * cell.height_px
                            + pad_rows * cell.height_px;
                        chrome.header_y = tail_view.active_top;
                        pane.record_frozen_chrome(chrome, item_top);
                    }
                });
                let tail_shaped = crate::terminal::block_list::shape_frozen_rows(
                    &tail_view.rows,
                    cell.width_px,
                    window,
                );
                let active_bounds = Bounds::new(
                    point(bounds.left(), bounds.top() + px(tail_view.active_top)),
                    size(bounds.size.width, px(live_rows as f32 * cell.height_px)),
                );
                let active_shaped = shape_frame(active_bounds, frame, *cell, window);
                BlockListItemPrepaint::Live {
                    tail_view,
                    tail_shaped,
                    active_shaped,
                }
            }
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Style,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        match (self, prepaint) {
            (
                BlockListItem::Frozen { cell, .. },
                BlockListItemPrepaint::Frozen { view, shaped },
            ) => {
                paint_frozen_images(bounds, view, *cell, window, false);
                crate::terminal::block_list::paint_frozen(
                    bounds,
                    view,
                    shaped,
                    cell.width_px,
                    cell.height_px,
                    window,
                    cx,
                );
                paint_frozen_images(bounds, view, *cell, window, true);
            }
            (
                BlockListItem::Live { frame, cell, .. },
                BlockListItemPrepaint::Live {
                    tail_view,
                    tail_shaped,
                    active_shaped,
                },
            ) => {
                crate::terminal::block_list::paint_frozen(
                    bounds,
                    tail_view,
                    tail_shaped,
                    cell.width_px,
                    cell.height_px,
                    window,
                    cx,
                );
                let active_bounds = Bounds::new(
                    point(bounds.left(), bounds.top() + px(tail_view.active_top)),
                    size(
                        bounds.size.width,
                        px(active_shaped.len() as f32 * cell.height_px),
                    ),
                );
                paint_frame(
                    active_bounds,
                    frame,
                    active_shaped.as_slice(),
                    *cell,
                    &[],
                    window,
                    cx,
                );
            }
            _ => {}
        }
    }
}

impl BlockListItem {
    fn pane(&self) -> &Entity<TerminalPane> {
        match self {
            BlockListItem::Frozen { pane, .. } | BlockListItem::Live { pane, .. } => pane,
        }
    }
}

/// Blank rows around each block for the current presentation: chrome shows
/// one pad row above and below; compact (Command Blocks off) packs block rows
/// contiguously like a classic grid. Every block-list geometry consumer must
/// use this one value per frame so heights, hit-testing, and scroll math agree.
fn block_pad_rows(cx: &App) -> f32 {
    if cx.global::<AppSettings>().command_blocks {
        crate::terminal::block_list::ITEM_PAD_ROWS
    } else {
        0.0
    }
}

fn frame_content_rows(frame: &TerminalFrame) -> usize {
    let lines = frame.lines();
    let mut content_end = 0;
    for (row, line) in lines.iter().enumerate().rev() {
        if terminal_line_has_content(line) {
            content_end = row + 1;
            break;
        }
    }
    if let Some(cursor) = frame.cursor() {
        content_end = content_end.max(cursor.row as usize + 1);
    }
    content_end.min(lines.len())
}

fn bottom_anchor_offsets(frame: &TerminalFrame, cell_height: f32, fixed_bottom: bool) -> Vec<f32> {
    if !fixed_bottom {
        return Vec::new();
    }
    let rows = frame.lines().len();
    let slack = rows.saturating_sub(frame_content_rows(frame)) as f32 * cell_height;
    if slack > 0.0 {
        vec![slack; rows]
    } else {
        Vec::new()
    }
}

fn block_list_alignment(fixed_bottom: bool) -> ListAlignment {
    if fixed_bottom {
        ListAlignment::Bottom
    } else {
        ListAlignment::Top
    }
}

#[derive(Clone, Copy, PartialEq)]
struct BlockListMeasureKey {
    /// (cols, cell height, pad rows) — pad rows toggling (Command Blocks
    /// on/off) changes every item height, so it must force a full remeasure.
    layout: (u32, f32, f32),
    store_len: usize,
    evicted_items: u64,
    last_item_px: f32,
    tail_px: f32,
    live_rows: usize,
}

struct BlockListRenderMetrics {
    store_len: usize,
    evicted_items: u64,
    item_count: usize,
    frozen_px: f32,
    /// The live item's history rows in pixels (active-grid scrollback above
    /// the live grid) — the "tail" position in scroll/active-top math.
    tail_px: f32,
    total_px: f32,
    offset_px: f32,
    last_item_px: f32,
}

fn block_list_render_metrics(
    store: &nmt_terminal::block_store::BlockStore,
    live_rows: usize,
    history_rows: u64,
    cols: u32,
    cell_h: f32,
    pad_rows: f32,
    offset: ListOffset,
) -> BlockListRenderMetrics {
    let items = store.items();
    let store_len = items.len();
    let item_count = store_len + 1;
    let mut frozen_px = 0.0;
    let mut offset_px = 0.0;
    let mut last_item_px = 0.0;

    for (ix, item) in items.iter().enumerate() {
        let item_px = crate::terminal::block_list::item_px(item, cols, cell_h, pad_rows);
        if ix < offset.item_ix {
            offset_px += item_px;
        }
        if ix + 1 == store_len {
            last_item_px = item_px;
        }
        frozen_px += item_px;
    }

    let tail_px = history_rows as f32 * cell_h;
    let total_px = frozen_px
        + crate::terminal::block_list::live_item_px(history_rows, live_rows, cell_h, pad_rows);
    if offset.item_ix >= item_count {
        offset_px = total_px;
    } else if offset.item_ix <= store_len {
        offset_px += offset.offset_in_item.as_f32();
    }

    BlockListRenderMetrics {
        store_len,
        evicted_items: store.evicted_items,
        item_count,
        frozen_px,
        tail_px,
        total_px,
        offset_px,
        last_item_px,
    }
}

fn block_list_live_chrome(
    live_index: usize,
    live_rows: usize,
    cell_h: f32,
    in_flight: Option<&InFlightBlock>,
    has_open_prompt: bool,
    selected: bool,
) -> Option<crate::terminal::block_list::FrozenItemChrome> {
    let running = in_flight.map(|block| (block.command.as_str(), block.started_at));
    if running.is_none() && !has_open_prompt {
        return None;
    }
    crate::terminal::block_list::live_chrome(live_index, live_rows, cell_h, running, selected)
}

fn offset_frozen_chrome(
    mut chrome: crate::terminal::block_list::FrozenItemChrome,
    item_top: f32,
) -> crate::terminal::block_list::FrozenItemChrome {
    chrome.top += item_top;
    chrome.bottom += item_top;
    chrome.header_y += item_top;
    chrome
}

/// Element-local top of the live grid: frozen items, then the live item's
/// top pad and tail rows. Computed from the per-frame metrics so it stays
/// valid even when the live item is outside List's prepaint overdraw.
fn block_list_active_top_px(
    frozen_px: f32,
    tail_px: f32,
    cell_h: f32,
    pad_rows: f32,
    scroll_top: f32,
) -> f32 {
    (frozen_px + pad_rows * cell_h + tail_px - scroll_top).max(0.0)
}

fn shift_selected_item_for_eviction(
    selected: Option<usize>,
    evicted_delta: usize,
    store_len: usize,
) -> Option<usize> {
    let selected = selected?;
    if selected < evicted_delta {
        None
    } else {
        let shifted = selected - evicted_delta;
        (shifted <= store_len).then_some(shifted)
    }
}

fn live_frame_text(frame: &TerminalFrame) -> Option<String> {
    let rows = frame_content_rows(frame);
    if rows == 0 {
        return None;
    }
    let mut lines = frame
        .lines()
        .iter()
        .take(rows)
        .map(terminal_line_plain_text)
        .collect::<Vec<_>>();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn terminal_line_plain_text(line: &TerminalLine) -> String {
    line.text().replace('\u{00a0}', " ").trim_end().to_string()
}

fn terminal_line_has_content(line: &TerminalLine) -> bool {
    line.cells()
        .iter()
        .any(|c| !matches!(c.ch, '\0' | ' ' | '\u{00a0}'))
}

/// The pixel y-offset for a viewport row (0 with no gaps / out of range).
fn row_y_offset(offsets: &[f32], row: usize) -> f32 {
    offsets.get(row).copied().unwrap_or(0.0)
}

/// Inverse of the offset mapping: the viewport row under a content-relative
/// pixel y. A pointer inside the gap above a block maps to the block's first row.
fn terminal_row_at_y(y: f32, cell_height: f32, offsets: &[f32]) -> u16 {
    if offsets.is_empty() {
        return (y / cell_height).floor().max(0.0) as u16;
    }
    for (row, off) in offsets.iter().enumerate() {
        if y < (row as f32 + 1.0) * cell_height + off {
            return row as u16;
        }
    }
    offsets.len().saturating_sub(1) as u16
}

pub(crate) const BLOCK_SUCCESS_COLOR: u32 = 0xa3be8c;
pub(crate) const BLOCK_FAILURE_COLOR: u32 = 0xbf616a;
pub(crate) const BLOCK_RUNNING_COLOR: u32 = 0x88c0d0;
pub(crate) const BLOCK_INPUT_COLOR: u32 = 0xebcb8b;
pub(crate) const BLOCK_SELECTED_TINT: u32 = 0xffffff0d;

pub(crate) fn block_separator_bounds(
    bounds: Bounds<Pixels>,
    y: Pixels,
    thickness: f32,
) -> Bounds<Pixels> {
    let left = bounds.left() - px(metrics::PADDING_PX);
    let right = bounds.right() + px(metrics::PADDING_PX);
    Bounds::new(point(left, y), size(right - left, px(thickness)))
}

/// First `max` chars of the command for the header label.
pub(crate) fn truncate_command(command: &str, max: usize) -> String {
    if command.chars().count() <= max {
        command.to_string()
    } else {
        let head: String = command.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

fn shape_frame(
    bounds: Bounds<Pixels>,
    frame: &TerminalFrame,
    cell: metrics::CellMetrics,
    window: &mut Window,
) -> Vec<ShapedLine> {
    let row_count =
        ((bounds.size.height.as_f32() / cell.height_px).ceil() as usize).min(frame.lines().len());
    shape_lines(
        frame
            .lines()
            .iter()
            .take(row_count)
            .map(|line| (line.text_hash(), line)),
        cell.width_px,
        window,
    )
}

/// Shape terminal lines with per-cell forced width, cached by the caller's
/// key — the one shaping path for live-frame rows and frozen block rows.
pub(crate) fn shape_lines<'a>(
    lines: impl Iterator<Item = (u64, &'a TerminalLine)>,
    cell_w: f32,
    window: &mut Window,
) -> Vec<ShapedLine> {
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());
    let base = style.to_run(0);
    lines
        .map(|(key, line)| {
            let runs = terminal_text_runs(line, &base);
            window.text_system().shape_line_by_hash(
                key,
                line.text().len(),
                font_size,
                &runs,
                Some(px(cell_w)),
                || line.text().clone(),
            )
        })
        .collect()
}

/// Build one styled `TextRun` per foreground run, inheriting font/size from the
/// base run and overriding only the color. Run byte-lengths sum to the row text.
pub(crate) fn terminal_text_runs(line: &TerminalLine, base: &TextRun) -> Vec<TextRun> {
    if line.runs().is_empty() {
        // Blank row: keep the single zero/whitespace run path GPUI already handles.
        let mut run = base.clone();
        run.len = line.text().len();
        return vec![run];
    }
    line.runs()
        .iter()
        .map(|run| {
            let mut text_run = base.clone();
            text_run.len = run.len;
            text_run.color = rgb(run.fg.rgb_u32()).into();
            if run.bold {
                text_run.font.weight = FontWeight::BOLD;
            }
            if run.italic {
                text_run.font.style = FontStyle::Italic;
            }
            if run.underline {
                text_run.underline = Some(UnderlineStyle {
                    thickness: px(1.0),
                    color: None,
                    wavy: false,
                });
            }
            if run.strikethrough {
                text_run.strikethrough = Some(StrikethroughStyle {
                    thickness: px(1.0),
                    color: None,
                });
            }
            text_run
        })
        .collect()
}

fn paint_frame(
    bounds: Bounds<Pixels>,
    frame: &TerminalFrame,
    lines: &[ShapedLine],
    cell: metrics::CellMetrics,
    offsets: &[f32],
    window: &mut Window,
    cx: &mut App,
) {
    use crate::terminal::frame::ZLayer;
    // Kitty images below cell backgrounds (z < i32::MIN/2).
    paint_frame_images(
        bounds,
        frame,
        ZLayer::BelowBackground,
        cell,
        offsets,
        window,
    );
    for (row, line) in frame.lines().iter().take(lines.len()).enumerate() {
        paint_line_backgrounds_at(
            bounds,
            line,
            row as f32 * cell.height_px + row_y_offset(offsets, row),
            cell.width_px,
            cell.height_px,
            window,
        );
    }
    // Kitty images above backgrounds, below cursor/text (i32::MIN/2 <= z < 0).
    paint_frame_images(bounds, frame, ZLayer::BelowText, cell, offsets, window);
    paint_cursor(bounds, frame.cursor(), cell, offsets, window);

    paint_glyph_rows(
        bounds,
        lines.iter().enumerate().map(|(row, line)| {
            (
                row as f32 * cell.height_px + row_y_offset(offsets, row),
                line,
            )
        }),
        cell.height_px,
        window,
        cx,
    );
    // Kitty images above cursor/text (z >= 0).
    paint_frame_images(bounds, frame, ZLayer::AboveText, cell, offsets, window);
}

/// Paint shaped glyph rows at caller-supplied element-local y offsets — the
/// one glyph-paint convention (left-aligned, no wrap, cell-height lines) for
/// live-frame rows and frozen block rows.
pub(crate) fn paint_glyph_rows<'a>(
    bounds: Bounds<Pixels>,
    rows: impl Iterator<Item = (f32, &'a ShapedLine)>,
    cell_h: f32,
    window: &mut Window,
    cx: &mut App,
) {
    for (y, line) in rows {
        let _ = line.paint(
            point(bounds.left(), bounds.top() + px(y)),
            px(cell_h),
            TextAlign::Left,
            None,
            window,
            cx,
        );
    }
}

/// Paint the frame's Kitty images whose z-index falls in `layer`, in engine order (no
/// per-paint sort or descriptor allocation). Each image's full texture is painted into
/// the source-expanded bounds and clipped to its destination by a content mask, so a
/// source crop needs no CPU cropping. A painted generation is marked
/// uploaded so its atlas tile is released once its last reference drops.
fn paint_frame_images(
    bounds: Bounds<Pixels>,
    frame: &TerminalFrame,
    layer: crate::terminal::frame::ZLayer,
    cell: metrics::CellMetrics,
    offsets: &[f32],
    window: &mut Window,
) {
    let images = frame.images();
    if images.is_empty() {
        return; // no graphics: zero work
    }
    for img in images {
        if img.z_layer() != layer {
            continue;
        }
        let top = img.top_row();
        let row_offset = if top >= 0 {
            row_y_offset(offsets, top as usize)
        } else {
            0.0
        };
        let Some((dest, source)) = img.destination(
            cell.width_px,
            cell.height_px,
            f32::from(bounds.left()),
            f32::from(bounds.top()),
            row_offset,
        ) else {
            continue;
        };
        paint_generation(window, dest, source, &img.generation);
    }
}

/// Paint a block-list item's frozen Kitty image slices whose z-layer is on
/// the requested side of the frozen text: `above_text == false` paints the below-text
/// slices (before `paint_frozen`), `true` the above-text slices (after). Uses the same
/// source-crop primitive as live images; clips to each slice's destination cell rect.
fn paint_frozen_images(
    bounds: Bounds<Pixels>,
    view: &crate::terminal::block_list::FrozenView,
    cell: metrics::CellMetrics,
    window: &mut Window,
    above_text: bool,
) {
    if view.images.is_empty() {
        return;
    }
    for img in &view.images {
        if (img.z >= 0) != above_text {
            continue;
        }
        let dest = [
            f32::from(bounds.left()) + img.col as f32 * cell.width_px,
            f32::from(bounds.top()) + img.y,
            img.width as f32 * cell.width_px,
            cell.height_px,
        ];
        paint_generation(window, dest, img.source, &img.generation);
    }
}

/// Paint one image generation's `source` crop into `dest` and mark it
/// uploaded (its atlas tile releases with the last reference) — the shared
/// tail of live-frame and frozen image painting. Degenerate crops are
/// skipped.
fn paint_generation(
    window: &mut Window,
    dest: [f32; 4],
    source: [f32; 4],
    generation: &crate::terminal::graphics::ImageGeneration,
) {
    let Some(full) = crate::terminal::graphics::expanded_full_bounds(dest, source) else {
        return;
    };
    paint_image_clipped(window, dest, full, generation.image().clone());
    generation.mark_uploaded();
}

/// Paint `image`'s full texture into `full` bounds, clipped to `dest` — the source-crop
/// primitive. GPUI intersects the mask with the element's existing overflow
/// mask, so viewport clipping is automatic.
fn paint_image_clipped(
    window: &mut Window,
    dest: [f32; 4],
    full: [f32; 4],
    image: std::sync::Arc<RenderImage>,
) {
    let to_bounds = |b: [f32; 4]| Bounds {
        origin: point(px(b[0]), px(b[1])),
        size: size(px(b[2]), px(b[3])),
    };
    let mask = ContentMask {
        bounds: to_bounds(dest),
    };
    window.with_content_mask(Some(mask), |w| {
        let _ = w.paint_image(to_bounds(full), Corners::default(), image, 0, false);
    });
}

/// Paint one line's background color runs at an element-local pixel `y`,
/// merging contiguous cells of equal background into single quads. Shared by
/// the live grid and the frozen block rows.
pub(crate) fn paint_line_backgrounds_at(
    bounds: Bounds<Pixels>,
    line: &TerminalLine,
    y: f32,
    cell_w: f32,
    cell_h: f32,
    window: &mut Window,
) {
    let mut run_start = 0u16;
    let mut run_width = 0u16;
    let mut run_color: Option<TerminalColor> = None;
    let flush = |start: u16, width: u16, color: Option<TerminalColor>, window: &mut Window| {
        let Some(color) = color else { return };
        if width == 0 {
            return;
        }
        window.paint_quad(fill(
            Bounds::new(
                point(
                    bounds.left() + px(start as f32 * cell_w),
                    bounds.top() + px(y),
                ),
                size(px(width as f32 * cell_w), px(cell_h)),
            ),
            rgb(color.rgb_u32()),
        ));
    };
    for cell_data in line.cells() {
        let width: u16 = if cell_data.wide == nmt_terminal::terminal::square::Wide::Wide {
            2
        } else {
            1
        };
        if run_color == cell_data.background && run_start + run_width == cell_data.col {
            run_width += width;
            continue;
        }
        flush(run_start, run_width, run_color, window);
        run_start = cell_data.col;
        run_width = width;
        run_color = cell_data.background;
    }
    flush(run_start, run_width, run_color, window);
}

fn paint_cursor(
    bounds: Bounds<Pixels>,
    cursor: Option<TerminalCursor>,
    cell: metrics::CellMetrics,
    offsets: &[f32],
    window: &mut Window,
) {
    let Some(cursor) = cursor else {
        return;
    };
    let y_offset = row_y_offset(offsets, cursor.row as usize);
    let Some(bounds) = cursor_bounds(bounds, cursor, cell, y_offset) else {
        return;
    };
    let color = match cursor.shape {
        CursorShape::Block => rgba(0xd8dee966),
        CursorShape::Beam | CursorShape::Underline => rgb(0xd8dee9),
        CursorShape::Hidden => return,
    };
    window.paint_quad(fill(bounds, color));
}

fn cursor_bounds(
    bounds: Bounds<Pixels>,
    cursor: TerminalCursor,
    cell: metrics::CellMetrics,
    y_offset: f32,
) -> Option<Bounds<Pixels>> {
    let x = bounds.left() + px(cursor.col as f32 * cell.width_px);
    let y = bounds.top() + px(cursor.row as f32 * cell.height_px + y_offset);
    let thickness = px((cell.width_px.min(cell.height_px) / 8.0)
        .round()
        .clamp(1.0, 2.0));
    Some(match cursor.shape {
        CursorShape::Block => Bounds::new(point(x, y), size(px(cell.width_px), px(cell.height_px))),
        CursorShape::Beam => Bounds::new(point(x, y), size(thickness, px(cell.height_px))),
        CursorShape::Underline => Bounds::new(
            point(x, y + px(cell.height_px) - thickness),
            size(px(cell.width_px), thickness),
        ),
        CursorShape::Hidden => return None,
    })
}

#[cfg(test)]
mod tests {
    use gpui::{Bounds, ListAlignment, ListOffset, ScrollDelta, point, px, size};
    use nmt_terminal::ansi::CursorShape;
    use nmt_terminal::event::BlockEvent;
    use nmt_terminal::ghostty::BlockHandle;
    use nmt_terminal::render_buffer::RenderBuffer;
    use nmt_terminal::selection::SelectionType;

    use super::{
        cursor_bounds, metrics, scrollbar_offset_for_thumb, scrollbar_thumb_geometry,
        selection_drag_started, selection_type_for_click_count, terminal_cell_at_position,
        terminal_scroll_lines,
    };
    use crate::terminal::frame::TerminalCursor;
    use crate::terminal::surface::{SurfaceCell, SurfaceCellSide};

    fn block_item(seq: u64, id: u64, rows: usize) -> BlockEvent {
        BlockEvent::EngineBlock {
            seq,
            handle: BlockHandle { id, generation: 1 },
            rows,
        }
    }

    #[test]
    fn repeated_clicks_choose_terminal_selection_modes() {
        assert_eq!(selection_type_for_click_count(1), SelectionType::Simple);
        assert_eq!(selection_type_for_click_count(2), SelectionType::Semantic);
        assert_eq!(selection_type_for_click_count(3), SelectionType::Lines);
        assert_eq!(selection_type_for_click_count(4), SelectionType::Lines);
    }

    #[test]
    fn block_list_active_top_survives_when_live_item_is_not_prepainted() {
        use nmt_terminal::block_store::BlockStore;

        let mut store = BlockStore::default();
        store.apply([block_item(1, 1, 1)]);
        // One 1-row item = 1 content row + 2 pad rows = 30px; the live grid
        // then starts after its own top pad (10px), minus the 5px scroll.
        let pad = crate::terminal::block_list::ITEM_PAD_ROWS;
        let frozen_px: f32 = store
            .items()
            .iter()
            .map(|item| crate::terminal::block_list::item_px(item, 80, 10.0, pad))
            .sum();
        assert_eq!(
            super::block_list_active_top_px(frozen_px, 0.0, 10.0, pad, 5.0),
            35.0
        );
        // Compact presentation: no pads anywhere, so the live grid starts
        // right after the frozen rows.
        let compact_px: f32 = store
            .items()
            .iter()
            .map(|item| crate::terminal::block_list::item_px(item, 80, 10.0, 0.0))
            .sum();
        assert_eq!(compact_px, 10.0, "1 content row, no pad rows");
        assert_eq!(
            super::block_list_active_top_px(compact_px, 0.0, 10.0, 0.0, 5.0),
            5.0
        );
    }

    #[test]
    fn block_list_render_metrics_resolve_scroll_once() {
        use nmt_terminal::block_store::BlockStore;

        let mut store = BlockStore::default();
        store.apply([block_item(1, 1, 1)]);

        let metrics = super::block_list_render_metrics(
            &store,
            2,
            1,
            80,
            10.0,
            crate::terminal::block_list::ITEM_PAD_ROWS,
            ListOffset {
                item_ix: 1,
                offset_in_item: px(3.0),
            },
        );

        assert_eq!(metrics.store_len, 1);
        assert_eq!(metrics.item_count, 2);
        assert_eq!(metrics.frozen_px, 30.0, "1 row + 2 pad rows");
        assert_eq!(metrics.tail_px, 10.0, "one live-history row");
        assert_eq!(
            metrics.total_px, 80.0,
            "frozen 30 + live (history 10 + 2 rows + 2 pads)"
        );
        assert_eq!(metrics.offset_px, 33.0);
        assert_eq!(metrics.last_item_px, 30.0);
    }

    #[test]
    fn selected_item_tracks_store_head_eviction() {
        assert_eq!(
            super::shift_selected_item_for_eviction(Some(4), 2, 10),
            Some(2)
        );
        assert_eq!(
            super::shift_selected_item_for_eviction(Some(1), 2, 10),
            None
        );
        assert_eq!(
            super::shift_selected_item_for_eviction(Some(10), 3, 7),
            Some(7),
            "old live index shifts to the new live index"
        );
        assert_eq!(
            super::shift_selected_item_for_eviction(Some(11), 3, 7),
            None
        );
    }

    #[test]
    fn block_list_alignment_follows_input_style_anchor() {
        assert_eq!(super::block_list_alignment(false), ListAlignment::Top);
        assert_eq!(super::block_list_alignment(true), ListAlignment::Bottom);
    }

    #[test]
    fn block_list_live_chrome_marks_idle_open_prompt() {
        let chrome = super::block_list_live_chrome(4, 2, 10.0, None, true, false).unwrap();
        assert_eq!(chrome.item, 4);
        assert_eq!(chrome.accent, super::BLOCK_INPUT_COLOR);
        assert_eq!(chrome.header, None);
        assert!(!chrome.selected);

        assert!(super::block_list_live_chrome(4, 2, 10.0, None, false, false).is_none());
    }

    #[test]
    fn frozen_chrome_offset_moves_header_with_item() {
        let chrome = crate::terminal::block_list::FrozenItemChrome {
            item: 0,
            top: 0.0,
            bottom: 40.0,
            header_y: 10.0,
            accent: super::BLOCK_SUCCESS_COLOR,
            header: Some("build · ✓".into()),
            selected: false,
        };

        let chrome = super::offset_frozen_chrome(chrome, 80.0);
        assert_eq!(
            (chrome.top, chrome.bottom, chrome.header_y),
            (80.0, 120.0, 90.0)
        );
    }

    #[test]
    fn bottom_anchor_offsets_pin_content_to_the_floor() {
        let frame =
            crate::terminal::frame::TerminalFrame::from_render_buffer(&RenderBuffer::new(80, 3));

        assert_eq!(
            super::bottom_anchor_offsets(&frame, 10.0, false),
            Vec::<f32>::new()
        );
        assert_eq!(super::bottom_anchor_offsets(&frame, 10.0, true), [30.0; 3]);
    }

    /// The gutter hit band covers the strip painted in the left padding plus a
    /// small tolerance into column 0 — and nothing else.
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
    fn cursor_bounds_cover_block_beam_and_underline() {
        let bounds = Bounds::new(point(px(10.0), px(20.0)), size(px(100.0), px(100.0)));
        let cell = metrics::CellMetrics {
            width_px: 8.0,
            height_px: 18.0,
        };

        let block = cursor_bounds(
            bounds,
            TerminalCursor {
                col: 2,
                row: 1,
                shape: CursorShape::Block,
            },
            cell,
            0.0,
        )
        .unwrap();
        assert_eq!(block.origin, point(px(26.0), px(38.0)));
        assert_eq!(block.size, size(px(8.0), px(18.0)));

        let beam = cursor_bounds(
            bounds,
            TerminalCursor {
                col: 2,
                row: 1,
                shape: CursorShape::Beam,
            },
            cell,
            0.0,
        )
        .unwrap();
        assert_eq!(beam.size.width, px(1.0));
        assert_eq!(beam.size.height, px(18.0));

        let underline = cursor_bounds(
            bounds,
            TerminalCursor {
                col: 2,
                row: 1,
                shape: CursorShape::Underline,
            },
            cell,
            0.0,
        )
        .unwrap();
        assert_eq!(underline.origin.y, px(55.0));
        assert_eq!(underline.size, size(px(8.0), px(1.0)));
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

    #[test]
    fn scrollbar_opacity_fades_after_linger() {
        assert_eq!(super::scrollbar_opacity(true, None), Some(1.0));
        assert_eq!(
            super::scrollbar_opacity(false, Some(super::SCROLLBAR_LINGER / 2)),
            Some(1.0)
        );

        let fading = super::scrollbar_opacity(
            false,
            Some(super::SCROLLBAR_LINGER + super::SCROLLBAR_FADE / 2),
        )
        .unwrap();
        assert!(fading > 0.0 && fading < 1.0);

        assert_eq!(
            super::scrollbar_opacity(false, Some(super::SCROLLBAR_LINGER + super::SCROLLBAR_FADE)),
            None
        );
    }

    #[test]
    fn scrollbar_thumb_stays_inside_track_with_long_history() {
        let (top, height) = scrollbar_thumb_geometry(10_000.0, 9_975.0, 25.0).unwrap();

        assert!(top + height <= 1.0, "thumb bottom was {}", top + height);
        assert_eq!(
            scrollbar_offset_for_thumb(10_000.0, 25.0, top),
            Some(9_975.0)
        );
    }
}
