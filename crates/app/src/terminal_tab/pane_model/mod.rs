pub(super) use crate::terminal_tab::pane_model::settings::FrameTheme;

pub(super) mod frame_cache;

pub(super) mod frame_record;

pub(super) mod key_action;

pub(super) mod frozen_hit_map;

pub(super) mod list_mirror;

pub(super) mod mouse;

pub(super) mod scroll;

pub(super) mod selection_geometry;

pub(super) mod viewport;

mod settings;

mod blocks;

mod links;

mod scrollbar_activity;

#[cfg(test)]
pub(super) mod test_session;

#[cfg(test)]
mod tests;

use nmt_config::colors::Colors;

use nmt_input::keyboard::ModifiersState;

use nmt_terminal::input::{TerminalKey, WheelDelta};

use nmt_terminal::links::{follows_link, resolve_link};

use nmt_terminal::selection::SelectionType;

use nmt_terminal::session::interaction::{
    CopyCompletion, InputOutcome, TerminalInteraction, selection_type_for_click_count,
};

use nmt_terminal::session::{
    HostEvent, InFlightBlock, SurfaceMouseButton, SurfaceMouseEventKind, SurfaceScreenCell,
};

use crate::terminal_tab::block_list::ITEM_PAD_ROWS;

use crate::terminal_tab::block_list::chrome::DurationLabels;

use crate::terminal_tab::block_list::{
    BlockListPoint, block_list_active_top_px, block_list_render_metrics, nav_item_top,
};

use crate::terminal_tab::dirty::DirtyState;

use crate::terminal_tab::frame::TerminalFrame;

use crate::terminal_tab::frame_source::TerminalFrameSource;

use crate::terminal_tab::layout::{bottom_anchor_offsets, frame_content_rows};

use crate::terminal_tab::metrics::CellMetrics;

use crate::terminal_tab::pane_model::blocks::ListPlan;

use crate::terminal_tab::pane_model::frame_cache::TerminalFrameCache;

use crate::terminal_tab::pane_model::frame_record::FrameRecord;

use crate::terminal_tab::pane_model::frozen_hit_map::FrozenHitMap;

use crate::terminal_tab::pane_model::key_action::{KeyOutcome, TextInput};

use crate::terminal_tab::pane_model::links::{LinkHit, LinkHover};

use crate::terminal_tab::pane_model::list_mirror::{BlockListMirror, ListOp, ListPosition};

use crate::terminal_tab::pane_model::mouse::{
    MouseInput, MouseOutcome, MouseRelease, WheelOutcome,
};

use crate::terminal_tab::pane_model::scroll::ScrollOutcome;

use crate::terminal_tab::pane_model::scrollbar_activity::ScrollbarActivity;

use crate::terminal_tab::pane_model::selection_geometry::selection_drag_started;

use crate::terminal_tab::pane_model::settings::{CursorShapeFailure, CursorShapeUpdate};

use crate::terminal_tab::pane_model::viewport::{LocalPoint, LocalRect, Viewport};

use crate::terminal_tab::settings::TerminalSettings;

pub(super) trait ClipboardAccess {
    fn read(&mut self) -> Option<String>;

    fn write(&mut self, text: String) -> bool;
}

pub(super) struct PaneController {
    pub source: TerminalFrameSource,
    pub interaction: TerminalInteraction,
    pub settings: TerminalSettings,
    pub pad_rows: f32,
    pub theme: FrameTheme,
    pub duration_labels: DurationLabels,
    pub frame_cache: TerminalFrameCache,
    dirty: DirtyState,
    pub cell_metrics: Option<CellMetrics>,
    pub content_size: (f32, f32),
    pub in_flight: Option<InFlightBlock>,
    pub open_prompt: bool,
    pub block_list: BlockListMirror,
    pub frozen: FrozenHitMap,

    /// Ignore pointer jitter until the press moves beyond the drag threshold.
    selection_origin: Option<LocalPoint>,

    pub scrollbar: ScrollbarActivity,
    links: LinkHover,
    pub viewport: Viewport,
    clipboard: Box<dyn ClipboardAccess>,
}

impl PaneController {
    pub(super) fn new(
        source: TerminalFrameSource,
        settings: TerminalSettings,
        theme: FrameTheme,
        duration_labels: DurationLabels,
        clipboard: Box<dyn ClipboardAccess>,
    ) -> Self {
        Self {
            source,
            interaction: TerminalInteraction::default(),
            pad_rows: if settings.command_blocks {
                ITEM_PAD_ROWS
            } else {
                0.0
            },
            settings,
            theme,
            duration_labels,
            frame_cache: TerminalFrameCache::default(),
            dirty: DirtyState::default(),
            cell_metrics: None,
            content_size: (0.0, 0.0),
            in_flight: None,
            open_prompt: false,
            block_list: BlockListMirror::default(),
            frozen: FrozenHitMap::default(),
            selection_origin: None,
            scrollbar: ScrollbarActivity::default(),
            links: LinkHover::default(),
            viewport: Viewport::default(),
            clipboard,
        }
    }

    pub(super) fn block_list_mode(&self) -> bool {
        self.source.session.engine_blocks() && !self.source.session.alt_screen()
    }

    pub(super) fn content_cols(&self) -> u32 {
        self.cell_metrics.map_or(80, |cell| {
            (self.content_size.0 / cell.width_px).floor().max(1.0) as u32
        })
    }

    pub(super) fn refresh_frame(&mut self) {
        self.interaction.poll_expansion(&self.source.session);

        let previous = self.frame_cache.reusable_frame();

        self.frame_cache
            .rebuild(self.source.frame(previous.as_ref(), &self.theme));

        self.update_viewport();

        if self.links.enabled
            && let Some(position) = self.links.position()
        {
            let hit = self.link_at_position(position);

            self.links.update(position, hit);
        }
    }

    pub(super) fn update_viewport(&mut self) {
        self.viewport = if self.block_list_mode() {
            Viewport::BlockList {
                scroll_px: self.block_list.scrollbar.0,
                max_scroll_px: self.block_list.scrollbar.1,
                active_top: self.frozen.active_top(),
                viewport_px: self.content_size.1,
            }
        } else {
            let frame = self.frame_cache.current().unwrap_or_default();

            Viewport::Grid {
                scrollbar: frame.scrollbar(),
                row_offsets: self
                    .cell_metrics
                    .map_or_else(Vec::new, |cell| {
                        bottom_anchor_offsets(&frame, cell.height_px, self.settings.fixed_bottom())
                    })
                    .into(),
            }
        };
    }

    pub(super) fn drain_host_events(&mut self) -> Vec<HostEvent> {
        let events = self.source.session.poll_events();

        for event in &events {
            match event {
                HostEvent::CommandFinished { .. } => {
                    self.frame_cache.invalidate();

                    self.refresh_blocks();
                }
                HostEvent::CommandStarted
                | HostEvent::PromptStarted
                | HostEvent::PromptBoundaryTrusted(false)
                | HostEvent::Exit => self.refresh_blocks(),
                _ => {}
            }
        }

        events
    }

    fn refresh_blocks(&mut self) {
        self.in_flight = self.source.session.in_flight_block();
        self.open_prompt = self.source.session.open_prompt_region();
    }

    pub(super) fn live_history_rows(&self, frame: &TerminalFrame) -> u64 {
        if !self.source.session.engine_blocks() {
            return 0;
        }

        let sb = frame.scrollbar();

        sb.total.saturating_sub(sb.len)
    }

    pub(super) fn block_list_point_at(&self, local: LocalPoint) -> Option<BlockListPoint> {
        if !self.block_list_mode() || local.y >= self.frozen.active_top() {
            return None;
        }

        let cell = self.cell_metrics?;

        self.frozen.hit_test(
            local.x,
            local.y,
            cell.width_px,
            cell.height_px,
            self.content_cols(),
            self.pad_rows,
        )
    }

    pub(super) fn prepare_block_list(
        &mut self,
        frame: &TerminalFrame,
        cell: CellMetrics,
        viewport_px: f32,
        position: ListPosition,
    ) -> Option<ListPlan> {
        if !self.block_list_mode() {
            return None;
        }

        let cols = self.content_cols();
        let live_rows = frame_content_rows(frame);
        let history_rows = self.live_history_rows(frame);
        let store = self.source.session.block_store();

        let metrics = block_list_render_metrics(
            &store.lock(),
            live_rows,
            history_rows,
            cols,
            cell.height_px,
            self.pad_rows,
            position,
        );

        let ops = self
            .block_list
            .sync(&metrics, (cols, cell.height_px, self.pad_rows), live_rows);

        let max_scroll = (metrics.total_px - viewport_px).max(0.0);
        let offset = metrics.offset_px.min(max_scroll);

        self.block_list.scrollbar = (offset, max_scroll);

        self.block_list.active_top = block_list_active_top_px(
            metrics.frozen_px,
            metrics.tail_px,
            cell.height_px,
            self.pad_rows,
            offset,
        );

        self.frozen.set_active_top(self.block_list.active_top);

        self.update_viewport();

        Some(ListPlan {
            ops,
            history_rows,
            live_index: metrics.store_len,
            cols,
        })
    }

    pub(super) fn record_frame(&mut self, record: FrameRecord) {
        for (y, item, row, cols) in record.rows {
            self.frozen.push_row(y, item, row, cols);
        }

        for y in record.separators {
            self.frozen.push_separator(y);
        }

        for chrome in record.chrome {
            self.frozen.push_chrome(chrome, 0.0);
        }

        if let Some(top) = record.active_top {
            self.frozen.set_active_top(top);

            self.update_viewport();
        }
    }

    pub(super) fn key_down(&mut self, key: &TerminalKey<'_>) -> KeyOutcome {
        if !self.source.session.alt_screen()
            && key.modifiers.is_empty()
            && !key.function
            && key.key.eq_ignore_ascii_case("end")
        {
            match self.scroll_to_latest() {
                ScrollOutcome::Ignored => {}
                outcome => return KeyOutcome::Scrolled(outcome),
            }
        }

        if self.source.session.defer_key_to_ime(key) {
            return KeyOutcome::Ignored;
        }

        self.send_key(key)
    }

    pub(super) fn send_key(&mut self, key: &TerminalKey<'_>) -> KeyOutcome {
        match self.interaction.send_key(
            &self.source.session,
            &self.source.snapshot,
            key,
            self.settings.newline_shortcut,
        ) {
            InputOutcome::Ignored => KeyOutcome::Ignored,
            InputOutcome::Written => KeyOutcome::Written,
            InputOutcome::CopyPending(copy) => KeyOutcome::CopyPending(copy),
            InputOutcome::PasteRequested => {
                let Some(text) = self.clipboard.read() else {
                    return KeyOutcome::Ignored;
                };

                if self.source.session.paste_text(&text) {
                    KeyOutcome::Written
                } else {
                    KeyOutcome::Ignored
                }
            }
        }
    }

    pub(super) fn write_text_input(&mut self, input: TextInput<'_>) -> bool {
        match input {
            TextInput::Commit(text) => self.source.session.commit_text(text),
            TextInput::DropPaths(paths) => self.source.session.paste_paths(paths),
        }
    }

    pub(super) fn copy_text_to_clipboard(&mut self, text: String) -> bool {
        !text.is_empty() && self.clipboard.write(text)
    }

    pub(super) fn finish_copy(&mut self, text: String, completion: CopyCompletion) -> bool {
        if !self.copy_text_to_clipboard(text) {
            return false;
        }

        self.interaction
            .complete_copy(&self.source.session, &self.source.snapshot, completion);

        true
    }

    pub(super) fn cell_metrics_or_measure(
        &mut self,
        measure: impl FnOnce() -> CellMetrics,
    ) -> CellMetrics {
        *self.cell_metrics.get_or_insert_with(measure)
    }

    pub(super) fn resize_content(&mut self, width: f32, height: f32, cell: CellMetrics) -> bool {
        self.content_size = (width, height);
        self.cell_metrics = Some(cell);

        self.update_viewport();

        let resized = self.source.resize_for_content(width, height, cell);

        if resized {
            self.frame_cache.invalidate();
        }

        resized
    }

    /// Retain the displayed frame and its coordinates until the next render.
    /// The return value reports a clean-to-dirty transition for wake coalescing.
    pub(super) fn invalidate(&mut self) -> bool {
        self.frame_cache.invalidate();

        self.dirty.mark()
    }

    pub(super) fn begin_frame(&mut self) -> TerminalFrame {
        self.dirty.begin_frame();

        if self.frame_cache.needs_rebuild() {
            self.refresh_frame();
        }

        self.frame_cache.current().unwrap_or_default()
    }

    pub(super) fn begin_block_list_frame(&mut self) {
        self.frozen.begin_frame(self.block_list.active_top);

        self.update_viewport();
    }

    pub(super) fn hovered_link(&self) -> Option<&LinkHit> {
        self.links.current()
    }

    pub(super) fn pointer_left(&mut self) -> bool {
        self.links.enabled = false;

        self.links.forget_position();

        self.links.clear()
    }

    pub(super) fn hover_modifiers_changed(&mut self, modifiers: ModifiersState) -> bool {
        self.links.enabled = follows_link(modifiers);

        let Some(position) = self.links.position() else {
            return false;
        };

        let hit = self
            .links
            .enabled
            .then(|| self.link_at_position(position))
            .flatten();

        self.links.update(position, hit)
    }

    fn hover_at(&mut self, position: LocalPoint, modifiers: ModifiersState) -> bool {
        if position.x < 0.0
            || position.y < 0.0
            || position.x > self.content_size.0
            || position.y > self.content_size.1
        {
            return self.pointer_left();
        }

        self.links.record_position(position);

        self.hover_modifiers_changed(modifiers)
    }

    /// Resolve the link under a pointer position: the row's OSC 8 span if one
    /// covers the pointed-at cell, else a URL-shaped token in the row text.
    /// Soft-wrapped neighbor rows are joined so long URLs match whole. Also
    /// yields underline rects (content-origin-relative) for hover feedback.
    pub(super) fn link_at_position(&self, position: LocalPoint) -> Option<LinkHit> {
        let cell_metrics = self.cell_metrics?;

        enum RowSource {
            Screen(i64),
            Block { item: usize, line: i64 },
        }

        let viewport_top = self.source.snapshot.viewport_top();

        let (source, col) = match self.block_list_point_at(position) {
            Some(BlockListPoint::Frozen(pt)) => (
                RowSource::Block {
                    item: pt.item,
                    line: pt.line as i64,
                },
                pt.col as usize,
            ),
            Some(BlockListPoint::LiveHistory { row, col }) => {
                (RowSource::Screen(row as i64), col as usize)
            }
            None => {
                let (cell, _) = self.viewport.cell_at(position, cell_metrics);

                (
                    RowSource::Screen(viewport_top? as i64 + cell.row as i64),
                    cell.col as usize,
                )
            }
        };

        let row_at = |delta: i64| match source {
            RowSource::Screen(row) => u32::try_from(row + delta).ok().and_then(|row| {
                self.source
                    .session
                    .screen_row_text_in(&self.source.snapshot, row)
            }),
            RowSource::Block { item, line, .. } => usize::try_from(line + delta)
                .ok()
                .and_then(|line| self.source.session.block_row_text(item, line)),
        };

        // Content-local y of the row `delta` rows below the pointed-at one;
        // `None` when it is scrolled out of view (that segment gets no rect).
        let row_y = |delta: i64| -> Option<f32> {
            match source {
                RowSource::Screen(row) => {
                    let row = row + delta;
                    let top = viewport_top? as i64;

                    if row < top {
                        // A live-history row above the engine viewport.
                        return self.frozen.row_top(usize::MAX, usize::try_from(row).ok()?);
                    }

                    Some(self.viewport.cursor_y(
                        (row - top).min(u16::MAX as i64) as u16,
                        cell_metrics.height_px,
                    ))
                }
                RowSource::Block { item, line, .. } => self
                    .frozen
                    .row_top(item, usize::try_from(line + delta).ok()?),
            }
        };

        let resolved = resolve_link(col, row_at)?;

        let rects = resolved
            .segments
            .into_iter()
            .filter_map(|segment| {
                let y = row_y(segment.delta)?;

                Some(LocalRect {
                    origin: LocalPoint {
                        x: segment.col as f32 * cell_metrics.width_px,
                        y: y + cell_metrics.height_px - 1.5,
                    },
                    width: segment.cols as f32 * cell_metrics.width_px,
                    height: 1.0,
                })
            })
            .collect();

        Some(LinkHit {
            url: resolved.url,
            rects,
        })
    }

    pub(super) fn mouse_down(&mut self, input: MouseInput) -> MouseOutcome {
        self.interaction.begin_pointer();

        self.selection_origin = None;

        let left = input.button == Some(SurfaceMouseButton::Left);

        if left
            && follows_link(input.modifiers)
            && let Some(link) = self.link_at_position(input.position)
        {
            return MouseOutcome::OpenUrl(link.url);
        }

        let mut cleared = false;

        let reports = self
            .source
            .session
            .mouse_reporting_active_for(input.modifiers);

        let kind = selection_type_for_click_count(input.click_count);

        self.selection_origin = (left && !reports).then_some(input.position);

        if self.block_list_mode() && !reports {
            if left
                && let Some(BlockListPoint::Frozen(point)) =
                    self.block_list_point_at(input.position)
            {
                self.interaction
                    .select_block(&self.source.session, point, kind);

                return MouseOutcome::FrozenSelectionStarted;
            }

            cleared |= self.interaction.clear_block_selection();
        }

        match self.apply_mouse(input, SurfaceMouseEventKind::Down, kind) {
            MouseOutcome::Ignored if cleared => MouseOutcome::SelectionChanged,
            outcome => outcome,
        }
    }

    pub(super) fn mouse_up(&mut self, input: MouseInput) -> MouseRelease {
        let scrollbar_released = self.scrollbar.end_drag();

        self.selection_origin = None;

        let outcome = if self.interaction.commit_block_selection() {
            MouseOutcome::Ignored
        } else {
            self.apply_mouse(input, SurfaceMouseEventKind::Up, SelectionType::Simple)
        };

        MouseRelease {
            outcome,
            scrollbar_released,
        }
    }

    pub(super) fn mouse_move(&mut self, input: MouseInput) -> MouseOutcome {
        let hover_changed = if input.button.is_none() {
            self.hover_at(input.position, input.modifiers)
        } else {
            self.links.record_position(input.position);

            false
        };

        match self.move_selection_or_scroll(input) {
            MouseOutcome::Ignored | MouseOutcome::Scrolled(ScrollOutcome::Ignored)
                if hover_changed =>
            {
                MouseOutcome::HoverChanged
            }
            outcome => outcome,
        }
    }

    fn move_selection_or_scroll(&mut self, input: MouseInput) -> MouseOutcome {
        if self.scrollbar.is_dragging() {
            let fraction = (input.position.y / self.content_size.1.max(1.0)).clamp(0.0, 1.0);

            return MouseOutcome::Scrolled(
                self.scroll_thumb_to(self.scrollbar.thumb_top_for(fraction)),
            );
        }

        if let Some(origin) = self.selection_origin {
            let Some(cell) = self.cell_metrics else {
                return MouseOutcome::Ignored;
            };

            if !selection_drag_started(origin, input.position, cell.width_px) {
                return MouseOutcome::Ignored;
            }

            self.selection_origin = None;
        }

        if self.interaction.block_anchor().is_some() {
            let mut position = input.position;

            position.y = position.y.min((self.frozen.active_top() - 1.0).max(0.0));

            if let Some(BlockListPoint::Frozen(head)) = self.block_list_point_at(position)
                && self.interaction.extend_block_selection(head)
            {
                return MouseOutcome::SelectionChanged;
            }

            return MouseOutcome::Ignored;
        }

        self.apply_mouse(input, SurfaceMouseEventKind::Move, SelectionType::Simple)
    }

    fn apply_mouse(
        &self,
        input: MouseInput,
        kind: SurfaceMouseEventKind,
        selection: SelectionType,
    ) -> MouseOutcome {
        let Some(metrics) = self.cell_metrics else {
            return MouseOutcome::Ignored;
        };

        let (cell, side) = self.viewport.cell_at(input.position, metrics);

        let handled = if self.block_list_mode()
            && !self
                .source
                .session
                .mouse_reporting_active_for(input.modifiers)
            && input.button == Some(SurfaceMouseButton::Left)
            && let Some(point) = self.block_list_point_at(input.position)
        {
            let screen = match point {
                BlockListPoint::LiveHistory { row, col } => SurfaceScreenCell { row, col },
                BlockListPoint::Frozen(point) => SurfaceScreenCell {
                    row: 0,
                    col: point.col.min(u16::MAX as u32) as u16,
                },
            };

            self.source
                .session
                .apply_screen_selection(screen, side, kind, selection)
        } else if input.button == Some(SurfaceMouseButton::Left)
            && !self
                .source
                .session
                .mouse_reporting_active_for(input.modifiers)
        {
            self.source.session.apply_screen_selection(
                SurfaceScreenCell {
                    col: cell.col,
                    row: self
                        .source
                        .snapshot
                        .viewport_top()
                        .unwrap_or(0)
                        .saturating_add(cell.row.into()),
                },
                side,
                kind,
                selection,
            )
        } else {
            self.source.session.apply_mouse(
                cell,
                side,
                input.button,
                kind,
                input.modifiers,
                selection,
            )
        };

        if handled {
            MouseOutcome::EngineHandled
        } else {
            MouseOutcome::Ignored
        }
    }

    pub(super) fn scroll_wheel(
        &mut self,
        position: LocalPoint,
        delta: WheelDelta,
        modifiers: ModifiersState,
    ) -> WheelOutcome {
        let lines = delta.lines();
        let hover_changed = self.links.clear();

        let mut outcome = WheelOutcome {
            handled: false,
            hover_changed,
        };

        if lines == 0 || (self.block_list_mode() && !self.source.session.mouse_reporting_active()) {
            return outcome;
        }

        let Some(metrics) = self.cell_metrics else {
            return outcome;
        };

        let (cell, _) = self.viewport.cell_at(position, metrics);

        outcome.handled = self.source.session.apply_scroll(cell, lines, modifiers);

        outcome
    }

    pub(super) fn scrollbar_mouse_down(
        &mut self,
        position: LocalPoint,
        thumb_top: f32,
        thumb_height: f32,
    ) -> ScrollOutcome {
        let fraction = (position.y / self.content_size.1.max(1.0)).clamp(0.0, 1.0);

        if (thumb_top..thumb_top + thumb_height).contains(&fraction) {
            self.scrollbar.begin_drag(fraction - thumb_top);

            ScrollOutcome::Ignored
        } else {
            self.scrollbar.begin_drag(thumb_height / 2.0);

            self.scroll_thumb_to(self.scrollbar.thumb_top_for(fraction))
        }
    }

    pub(super) fn scroll_to_latest(&mut self) -> ScrollOutcome {
        if !self.viewport.is_scrolled() {
            return ScrollOutcome::Ignored;
        }

        match self.viewport {
            Viewport::BlockList { .. } => {
                self.block_list.scrollbar.0 = self.block_list.scrollbar.1;

                self.update_viewport();

                ScrollOutcome::List(ListOp::ScrollToEnd)
            }
            Viewport::Grid { .. } => self.scroll_thumb_to(1.0),
        }
    }

    pub(super) fn scroll_thumb_to(&mut self, thumb_top: f32) -> ScrollOutcome {
        let Some(target) = self.viewport.thumb_target(thumb_top) else {
            return ScrollOutcome::Ignored;
        };

        match &self.viewport {
            Viewport::BlockList { .. } => self.scroll_list_to(target as f32),
            Viewport::Grid { .. } => {
                let accepted = if thumb_top >= 1.0 {
                    self.source.session.scroll_to_end()
                } else {
                    self.source
                        .session
                        .scroll_to(target.round().max(0.0) as u64)
                };

                if accepted {
                    ScrollOutcome::GridRequested
                } else {
                    ScrollOutcome::Ignored
                }
            }
        }
    }

    fn scroll_list_to(&mut self, target: f32) -> ScrollOutcome {
        let (Some(frame), Some(cell)) = (self.frame_cache.current(), self.cell_metrics) else {
            return ScrollOutcome::Ignored;
        };

        let store = self.source.session.block_store();

        let op = BlockListMirror::scroll_to_px(
            &store.lock(),
            self.live_history_rows(&frame),
            frame_content_rows(&frame),
            (self.content_cols(), cell.height_px, self.pad_rows),
            target,
        );

        self.block_list.scrollbar.0 = target.min(self.block_list.scrollbar.1);

        self.update_viewport();

        ScrollOutcome::List(op)
    }

    pub(super) fn jump_to_block(&mut self, direction: i8) -> ScrollOutcome {
        let Some(cell) = self.cell_metrics else {
            return ScrollOutcome::Ignored;
        };

        let store = self.source.session.block_store();

        let target = nav_item_top(
            &store.lock(),
            self.content_cols(),
            cell.height_px,
            self.pad_rows,
            self.block_list.scrollbar.0,
            direction,
        );

        target.map_or(ScrollOutcome::Ignored, |target| self.scroll_list_to(target))
    }

    pub(super) fn update_settings(
        &mut self,
        settings: TerminalSettings,
        colors: &Colors,
        duration_labels: DurationLabels,
    ) -> Option<CursorShapeUpdate> {
        self.source.session.set_theme_colors(colors);

        if settings.improve_powershell_compatibility
            != self.settings.improve_powershell_compatibility
        {
            self.source
                .session
                .set_powershell_compatibility(settings.improve_powershell_compatibility);
        }

        let cursor_update =
            (settings.cursor_shape != self.settings.cursor_shape).then(|| CursorShapeUpdate {
                request: self.source.session.set_cursor_shape(settings.cursor_shape),
                failure: CursorShapeFailure {
                    previous: self.settings.cursor_shape,
                    requested: settings.cursor_shape,
                },
            });

        self.pad_rows = if settings.command_blocks {
            ITEM_PAD_ROWS
        } else {
            0.0
        };

        self.settings = settings;
        self.theme = colors.into();
        self.duration_labels = duration_labels;
        self.cell_metrics = None;

        self.frame_cache.invalidate_full();

        cursor_update
    }

    pub(super) fn cursor_shape_failed(&mut self, failure: CursorShapeFailure) -> bool {
        if self.settings.cursor_shape != failure.requested {
            return false;
        }

        self.settings.cursor_shape = failure.previous;

        self.invalidate();

        true
    }
}
