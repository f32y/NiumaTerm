use std::sync::atomic::{AtomicBool, Ordering};

use nmt_config::local_state::TabState;
use nmt_input::keyboard::ModifiersState;
use nmt_terminal::render_buffer::RenderBuffer;
use nmt_terminal::selection::{Selection, SelectionRange, SelectionType, WORD_DELIMITERS};
use nmt_terminal::terminal::Mode;
use nmt_terminal::terminal::pos::{Column, Line, Pos, Side};
use parking_lot::Mutex;

use crate::terminal::frame::TerminalFrame;
use crate::terminal::metrics;
use crate::terminal::session::{HostEvent, TerminalSession, TerminalSessionConfig};
use crate::terminal::wake::{Wake, WakeSender, WakeSignal};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalKeyAction {
    Write(Vec<u8>),
    CopyOrWrite(Vec<u8>),
    Paste,
    Ignore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SurfaceCell {
    pub(crate) col: u16,
    pub(crate) row: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SurfaceScreenCell {
    pub(crate) col: u16,
    pub(crate) row: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SurfaceCellSide {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SurfaceMouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SurfaceMouseEventKind {
    Down,
    Up,
    Move,
}

/// One row read for pointer URL hit-testing: plain text padded to the grid
/// width so char index == grid column (only each cell's first codepoint is
/// kept — grapheme extras would break the column mapping), the row's OSC 8
/// spans, and its soft-wrap flag.
pub(crate) struct PointerRow {
    pub(crate) text: String,
    pub(crate) wrapped: bool,
    /// OSC 8 spans: `(start_col, end_col_inclusive, uri)`.
    pub(crate) hyperlinks: Vec<(u16, u16, String)>,
}

pub struct TerminalSurface {
    session: TerminalSession,
    launch_state: TabState,
    last_cwd: Mutex<Option<String>>,
    selection: Mutex<Option<Selection>>,
    read_only: AtomicBool,
    /// A full-screen program owns the grid (alt-screen). A subset of
    /// [`Self::interactive`] — excludes the unmarked-prompt fallback — used to
    /// suppress command-block chrome only when the whole grid is repainted by a
    /// TUI, keeping blocks visible during plain interactive prompts.
    alt_screen: AtomicBool,
    /// Current grid size, tracked so [`Self::resize_for_content`] can skip
    /// no-op resizes when the laid-out area maps to the same cell grid.
    grid_size: (u16, u16),
}

impl TerminalSurface {
    pub(crate) fn title(&self) -> String {
        self.session.engine.lock().title()
    }

    pub(crate) fn set_theme_colors(&self, colors: &nmt_config::colors::Colors) {
        self.session.engine.lock().set_theme_colors(colors);
    }

    pub fn new(
        config: TerminalSessionConfig,
        id: u64,
        wake: Option<WakeSender>,
    ) -> Result<Self, String> {
        let grid_size = (config.cols, config.rows);
        let launch_state = config.restorable_tab_state();
        let config = config.with_shell_integration();
        let session = TerminalSession::new(&config, id, wake)
            .map_err(|error| format!("{:?}: {}", error.code, error))?;

        Ok(Self {
            session,
            launch_state,
            last_cwd: Mutex::new(None),
            selection: Mutex::new(None),
            read_only: AtomicBool::new(false),
            alt_screen: AtomicBool::new(false),
            grid_size,
        })
    }

    /// Build a surface for the GPUI shell: a fixed initial grid and PTY wakeups
    /// posted through the GPUI `wake` signal. Finished commands stay in engine
    /// blocks so the UI can change their presentation without losing output.
    pub(crate) fn for_gpui(
        wake: WakeSignal,
        surface_id: u64,
        shell: Option<String>,
        args: Vec<String>,
        working_dir: Option<String>,
        starting_title: String,
        fixed_bottom_requested: bool,
        environment_overrides: Vec<(String, String)>,
    ) -> Result<Self, String> {
        let wake_sender = WakeSender::from_fn(move |_kind: Wake| {
            wake.signal();
        });
        let config = TerminalSessionConfig {
            shell: shell.clone(),
            args: args.clone(),
            working_dir: working_dir.clone(),
            starting_title: Some(starting_title),
            cols: metrics::COLS,
            rows: metrics::ROWS,
            environment_overrides,
            ..TerminalSessionConfig::default()
        };
        let _ = fixed_bottom_requested;
        Self::new(config, surface_id, Some(wake_sender))
    }

    /// Number of processes beyond the shell itself in the shell's Job
    /// Object (requires the job-management setting; 0 otherwise).
    /// Shared frozen block-split history (renderer read side).
    pub(crate) fn block_store(
        &self,
    ) -> std::sync::Arc<parking_lot::Mutex<nmt_terminal::block_store::BlockStore>> {
        self.session.block_store()
    }

    /// In engine-blocks mode, frozen history items are finished
    /// engine blocks, rendered through `BlockRef` handles.
    pub(crate) fn engine_blocks(&self) -> bool {
        self.session.engine_blocks()
    }

    /// Acquire a read reference to a finished engine block, plus the palette
    /// styles resolve against and the block's Kitty placements in
    /// block-relative coordinates. Takes the engine lock only
    /// for the acquire itself; every text read through the returned
    /// reference is lock-free. `None` for a stale handle (evicted) or while
    /// the engine is reflowing the block — retry next frame.
    ///
    /// Lock discipline: never call this while holding the block-store lock
    /// (the PTY thread nests engine → store; nesting store → engine here
    /// would deadlock).
    pub(crate) fn acquire_block(
        &self,
        handle: nmt_terminal::ghostty::BlockHandle,
    ) -> Option<nmt_terminal::ghostty::AcquiredBlock> {
        self.session.engine.lock().acquire_block_snapshot(handle)
    }

    /// The absolute SCREEN row of the viewport's top row, mapping pointer
    /// viewport rows into SCREEN space for URL hit-testing.
    pub(crate) fn viewport_top_screen_row(&self) -> Option<u32> {
        self.session.engine.lock().viewport_top_screen()
    }

    /// Read one absolute SCREEN row for pointer URL hit-testing.
    pub(crate) fn pointer_screen_row(&self, row: u32) -> Option<PointerRow> {
        let engine = self.session.engine.lock();
        let palette = engine.color_palette();
        let cols = engine.cols() as usize;
        let mut chars: Vec<char> = Vec::with_capacity(cols);
        let meta = engine
            .read_screen_row_visit(row, &palette, |x, text, _wide, _style| {
                push_pointer_cell(&mut chars, x, text.as_str());
            })
            .ok()
            .flatten()?;
        Some(pointer_row(chars, cols, meta))
    }

    /// Read one row of a finished engine block for pointer URL hit-testing.
    pub(crate) fn pointer_block_row(
        &self,
        handle: nmt_terminal::ghostty::BlockHandle,
        row: usize,
    ) -> Option<PointerRow> {
        let engine = self.session.engine.lock();
        let palette = engine.color_palette();
        let cols = engine.block_cols(handle).unwrap_or_else(|| engine.cols()) as usize;
        let mut chars: Vec<char> = Vec::with_capacity(cols);
        let meta = engine
            .read_block_row_visit(handle, row, &palette, |x, text, _wide, _style| {
                push_pointer_cell(&mut chars, x, text.as_str());
            })
            .ok()
            .flatten()?;
        Some(pointer_row(chars, cols, meta))
    }

    /// The cached frozen generation for `(block_id, image_id)`, if a paint
    /// already read it out of the engine block, avoiding eager image uploads.
    pub(crate) fn frozen_image(
        &self,
        block_id: u64,
        image_id: u32,
    ) -> Option<std::sync::Arc<crate::terminal::graphics::ImageGeneration>> {
        self.session
            .frozen_images()
            .lock()
            .get(&(block_id, image_id))
            .cloned()
    }

    pub(crate) fn insert_frozen_image(
        &self,
        block_id: u64,
        image_id: u32,
        generation: std::sync::Arc<crate::terminal::graphics::ImageGeneration>,
    ) {
        self.session
            .frozen_images()
            .lock()
            .insert((block_id, image_id), generation);
    }

    /// Read one frozen image's pixels out of an acquired block and build a
    /// paintable generation. The caller caches it under `(block_id, image_id)` in the
    /// block store, so each frozen image uploads lazily at most once
    /// per frozen image.
    pub(crate) fn frozen_image_generation(
        &self,
        block: &nmt_terminal::ghostty::BlockRef,
        image_id: u32,
    ) -> Option<std::sync::Arc<crate::terminal::graphics::ImageGeneration>> {
        let release = self.session.generation_store().lock().release_queue();
        let data = {
            let engine = self.session.engine.lock();
            engine.block_image_pixels(block, image_id)?
        };
        crate::terminal::graphics::graphic_to_generation(data, &release)
    }

    /// Engine-blocks mode: read active-grid scrollback rows (SCREEN
    /// coordinates) for the live item's scrolled-up history, replacing the harvested
    /// `Tail`. One engine lock hold covers the whole
    /// visible range; each row materializes as a display line.
    pub(crate) fn live_history_lines(
        &self,
        rows: std::ops::Range<u64>,
    ) -> Vec<(u64, crate::terminal::frame::TerminalLine)> {
        if rows.is_empty() {
            return Vec::new();
        }
        let engine = self.session.engine.lock();
        let palette = engine.color_palette();
        let default_fg = crate::terminal::frame::theme_default_foreground();
        rows.filter_map(|row| {
            let mut builder = crate::terminal::block_list::EngineRowBuilder::default();
            engine
                .read_screen_row_visit(row.min(u32::MAX as u64) as u32, &palette, |x, t, w, s| {
                    builder.push(x, t, w, &s, default_fg)
                })
                .ok()
                .flatten()
                .map(|_| (row, builder.finish()))
        })
        .collect()
    }

    /// Whether any live Kitty image generation exists (lock-free). Gates the atlas
    /// drain and the frame-path generation resolution so a graphics-free session
    /// pays nothing.
    pub(crate) fn has_live_images(&self) -> bool {
        self.session.has_live_images()
    }

    /// Take the GPUI images whose final reference dropped (replaced, removed, evicted,
    /// or lost their last frozen owner) so the caller can release their atlas tiles via
    /// `Window::drop_image`. Drains both live and frozen releases (one
    /// shared queue).
    pub(crate) fn drain_released_images(&self) -> Vec<std::sync::Arc<gpui::RenderImage>> {
        self.session.generation_store().lock().drain_released()
    }

    pub(crate) fn child_process_count(&self) -> usize {
        self.session.child_process_count()
    }

    pub fn poll_events(&self) -> Vec<HostEvent> {
        self.session.poll_events()
    }

    /// The in-flight command, if one is executing.
    pub(crate) fn in_flight_block(&self) -> Option<crate::terminal::session::InFlightBlock> {
        self.session.in_flight_block()
    }

    pub(crate) fn open_prompt_region(&self) -> bool {
        self.session.open_prompt_region()
    }

    /// Whether a PTY mouse-reporting mode is active.
    pub(crate) fn mouse_reporting_active(&self) -> bool {
        self.mouse_mode().is_some()
    }

    pub(crate) fn mouse_reporting_active_for(&self, modifiers: ModifiersState) -> bool {
        self.app_mouse_mode(modifiers).is_some()
    }

    /// Copy `text` to the system clipboard (block copy actions).
    pub(crate) fn copy_text_to_clipboard(&self, text: String) {
        if text.is_empty() {
            return;
        }
        let mut clipboard = nmt_terminal::clipboard::Clipboard::default();
        clipboard.set(nmt_terminal::clipboard::ClipboardType::Clipboard, text);
    }

    pub(crate) fn set_last_cwd(&self, cwd: String) {
        *self.last_cwd.lock() = Some(normalize_osc7_pwd(&cwd));
    }

    pub(crate) fn tab_state(&self) -> TabState {
        tab_state_with_cwd(&self.launch_state, self.last_cwd.lock().clone())
    }

    pub fn write_text(&self, text: &str) {
        self.write_bytes(text.as_bytes());
    }

    pub fn write_bytes(&self, bytes: &[u8]) {
        if bytes.is_empty() || self.read_only.load(Ordering::Relaxed) {
            return;
        }
        self.scroll_viewport_bottom_before_input();
        self.session.write_input(bytes);
    }

    pub(crate) fn apply_key_action(&self, action: TerminalKeyAction) -> bool {
        match action {
            TerminalKeyAction::Write(bytes) => {
                if bytes.is_empty() || self.read_only.load(Ordering::Relaxed) {
                    return false;
                }
                self.write_bytes(&bytes);
                true
            }
            TerminalKeyAction::CopyOrWrite(bytes) => {
                if self.copy_selection() {
                    return true;
                }
                if bytes.is_empty() || self.read_only.load(Ordering::Relaxed) {
                    return false;
                }
                self.write_bytes(&bytes);
                true
            }
            TerminalKeyAction::Paste => self.paste(),
            TerminalKeyAction::Ignore => false,
        }
    }

    pub(crate) fn apply_mouse(
        &self,
        cell: SurfaceCell,
        side: SurfaceCellSide,
        button: Option<SurfaceMouseButton>,
        kind: SurfaceMouseEventKind,
        modifiers: ModifiersState,
        selection_type: SelectionType,
    ) -> bool {
        if let Some(mode) = self.app_mouse_mode(modifiers) {
            return match kind {
                SurfaceMouseEventKind::Down | SurfaceMouseEventKind::Up => {
                    let Some(code) = button.and_then(mouse_button_code) else {
                        return false;
                    };
                    self.report_mouse(
                        mode,
                        code,
                        kind == SurfaceMouseEventKind::Down,
                        cell.col,
                        cell.row,
                        modifiers,
                    )
                }
                SurfaceMouseEventKind::Move => {
                    if button != Some(SurfaceMouseButton::Left)
                        || !mode.intersects(Mode::MOUSE_MOTION | Mode::MOUSE_DRAG)
                    {
                        return false;
                    }
                    self.report_mouse(mode, 32, true, cell.col, cell.row, modifiers)
                }
            };
        }

        if button != Some(SurfaceMouseButton::Left) {
            return false;
        }
        let pos = Pos::new(Line(cell.row as i32), Column(cell.col as usize));
        self.apply_selection_at(self.screen_pos(pos), side, kind, selection_type)
    }

    /// Apply a selection gesture to an absolute SCREEN cell. The block-list
    /// live history is rendered outside the engine viewport, but its rows keep
    /// these coordinates so selection and copy still use Ghostty's formatter.
    pub(crate) fn apply_screen_selection(
        &self,
        cell: SurfaceScreenCell,
        side: SurfaceCellSide,
        kind: SurfaceMouseEventKind,
        selection_type: SelectionType,
    ) -> bool {
        let Ok(row) = i32::try_from(cell.row) else {
            return false;
        };
        self.apply_selection_at(
            Pos::new(Line(row), Column(cell.col as usize)),
            side,
            kind,
            selection_type,
        )
    }

    pub(crate) fn frozen_selection_range(
        &self,
        handle: nmt_terminal::ghostty::BlockHandle,
        line: usize,
        col: u32,
        selection_type: SelectionType,
    ) -> Option<((usize, u32), (usize, u32))> {
        let engine = self.session.engine.lock();
        let palette = engine.color_palette();
        let block = engine.block_acquire(handle)?;
        block_selection_range(&block, &palette, line, col, selection_type)
    }

    pub(crate) fn apply_scroll(
        &self,
        cell: SurfaceCell,
        lines: i32,
        modifiers: ModifiersState,
    ) -> bool {
        if lines == 0 {
            return false;
        }
        if let Some(mode) = self.mouse_mode() {
            let button = if lines > 0 { 64 } else { 65 };
            return self.report_mouse(mode, button, true, cell.col, cell.row, modifiers);
        }
        self.scroll_lines(-(lines as isize))
    }

    pub(crate) fn scroll_lines(&self, delta: isize) -> bool {
        if delta == 0 {
            return false;
        }
        let before = self.session.render_buffer.lock().scrollbar();
        if before.total <= before.len {
            return false;
        }
        let snap = {
            let mut engine = self.session.engine.lock();
            engine.scroll_viewport_delta(delta);
            engine.snapshot()
        };
        let Ok(mut next) = snap else {
            return false;
        };
        let changed = next.scrollbar() != before;
        let mut buf = self.session.render_buffer.lock();
        next.set_cursor_visible(buf.cursor_visible());
        std::mem::swap(&mut *buf, &mut next);
        changed
    }

    pub fn resize(&self, cols: u16, rows: u16, width: u16, height: u16) {
        self.session.resize(cols, rows, width, height);
    }

    /// Resize from a content rect (padding already excluded), e.g. the terminal
    /// leaf's laid-out bounds when chrome occupies part of the window. Skips the
    /// resize when the area maps to the same cell grid.
    pub(crate) fn resize_for_content(
        &mut self,
        width_px: f32,
        height_px: f32,
        cell: metrics::CellMetrics,
    ) -> bool {
        let (cols, rows) = cell.grid_size_for_content(width_px, height_px);
        if self.grid_size == (cols, rows) {
            return false;
        }
        self.grid_size = (cols, rows);
        self.resize(
            cols,
            rows,
            metrics::pixel_u16(width_px),
            metrics::pixel_u16(height_px),
        );
        true
    }

    pub(crate) fn frame(&self, previous: Option<&TerminalFrame>) -> TerminalFrame {
        let total_start = std::time::Instant::now();
        let selection = self.selection_range();
        // Resolve live image generations before taking the render-buffer lock so the
        // generation-store and render locks are never nested. A graphics-free
        // session skips the store entirely via the lock-free live-image check, so it
        // pays nothing here.
        let generations = if self.session.has_live_images() {
            self.session.generation_store().lock().live_generations()
        } else {
            std::collections::HashMap::new()
        };
        let sel_us = total_start.elapsed().as_micros();
        let frame = self.with_render_buffer(|buf| {
            // Time spent here is *after* the render_buffer lock is acquired, so
            // (total - sel - extract) is the lock-wait + selection lock cost.
            let extract_start = std::time::Instant::now();
            let frame =
                TerminalFrame::from_render_buffer_reusing(buf, selection, &generations, previous);
            let extract_us = extract_start.elapsed().as_micros();
            tracing::trace!(
                target: "perf",
                rows = buf.rows(),
                cols = buf.cols(),
                extract_us,
                "frame extract (inside render_buffer lock)"
            );
            frame
        });
        tracing::trace!(
            target: "perf",
            sel_us,
            total_us = total_start.elapsed().as_micros(),
            "frame total (selection + lock-wait + extract)"
        );
        frame
    }

    pub fn with_render_buffer<R>(&self, read: impl FnOnce(&RenderBuffer) -> R) -> R {
        let buf = self.session.render_buffer.lock();
        read(&buf)
    }

    /// The selection mapped to visible-row coordinates for the frame highlight.
    /// Anchors are SCREEN coordinates (content-stable across scrolling), so the
    /// current `viewport_top` re-bases them; rows outside the viewport are
    /// clipped per-row by `row_selection_for`.
    pub(crate) fn selection_range(&self) -> Option<SelectionRange> {
        self.selection_range_at(self.viewport_top())
    }

    /// Selection in absolute SCREEN coordinates for live-history rows rendered
    /// above the pinned engine viewport.
    pub(crate) fn selection_screen_range(&self) -> Option<SelectionRange> {
        let viewport_top = self.viewport_top();
        let guard = self.selection.lock();
        let selection = guard.as_ref()?;
        let buf = self.session.render_buffer.lock();
        selection_screen_range(selection, &buf, viewport_top)
    }

    fn selection_range_at(&self, viewport_top: i32) -> Option<SelectionRange> {
        let guard = self.selection.lock();
        let sel = guard.as_ref()?;
        let buf = self.session.render_buffer.lock();
        sel.to_range_engine(&buf, viewport_top, WORD_DELIMITERS)
    }

    /// SCREEN row of the top visible row (0 when the viewport is empty).
    fn viewport_top(&self) -> i32 {
        self.session
            .engine
            .lock()
            .viewport_top_screen()
            .unwrap_or(0) as i32
    }

    pub fn alt_screen(&self) -> bool {
        self.alt_screen.load(Ordering::Relaxed)
    }

    pub fn set_alt_screen(&self, on: bool) -> bool {
        self.alt_screen.swap(on, Ordering::Relaxed) != on
    }

    pub fn mark_read_only(&self) {
        self.read_only.store(true, Ordering::Relaxed);
    }

    fn scroll_viewport_bottom_before_input(&self) {
        let should_scroll = {
            let sb = self.session.render_buffer.lock().scrollbar();
            should_scroll_to_bottom_before_input(sb.offset, sb.total, sb.len)
        };
        if should_scroll {
            self.session.engine.lock().scroll_viewport_bottom();
        }
    }

    /// Re-base a viewport-relative cell onto SCREEN coordinates, so the anchor
    /// stays on the same content when the viewport scrolls.
    fn screen_pos(&self, pos: Pos) -> Pos {
        Pos::new(Line(pos.row.0 + self.viewport_top()), pos.col)
    }

    /// Drop the engine-region selection (block-split: a frozen-region
    /// selection replaces it, and vice versa).
    pub(crate) fn clear_selection(&self) {
        *self.selection.lock() = None;
    }

    fn apply_selection_at(
        &self,
        pos: Pos,
        side: SurfaceCellSide,
        kind: SurfaceMouseEventKind,
        selection_type: SelectionType,
    ) -> bool {
        let side = match side {
            SurfaceCellSide::Left => Side::Left,
            SurfaceCellSide::Right => Side::Right,
        };
        match kind {
            SurfaceMouseEventKind::Down => self.begin_selection(pos, side, selection_type),
            SurfaceMouseEventKind::Move => self.update_selection(pos, side),
            SurfaceMouseEventKind::Up => self.finish_selection(),
        }
    }

    fn begin_selection(&self, pos: Pos, side: Side, selection_type: SelectionType) -> bool {
        let mut guard = self.selection.lock();
        let had_selection = guard.is_some();
        *guard = Some(Selection::new(selection_type, pos, side));
        had_selection || selection_type != SelectionType::Simple
    }

    fn update_selection(&self, pos: Pos, side: Side) -> bool {
        let mut guard = self.selection.lock();
        let Some(selection) = guard.as_mut() else {
            return false;
        };
        selection.update(pos, side);
        true
    }

    fn finish_selection(&self) -> bool {
        let mut guard = self.selection.lock();
        if guard.as_ref().is_some_and(Selection::is_empty) {
            *guard = None;
        }
        false
    }

    /// Selected text via the engine formatter. Ranges reaching into scrollback
    /// extract real content instead of stopping at the viewport.
    fn selection_text(&self) -> Option<String> {
        let range = self.selection_screen_range()?;
        if range.start.row.0 < 0 || range.end.row.0 < 0 {
            return None;
        }
        self.session
            .engine
            .lock()
            .format_screen_range(
                (range.start.col.0 as u16, range.start.row.0 as u32),
                (range.end.col.0 as u16, range.end.row.0 as u32),
                range.is_block,
                // Rejoin soft-wrapped lines and drop trailing blanks, matching
                // the prior hand-rolled trim behavior.
                true,
                true,
            )
            .ok()
    }

    fn copy_selection(&self) -> bool {
        let Some(text) = self.selection_text() else {
            return false;
        };
        if text.is_empty() {
            return false;
        }
        let mut clipboard = nmt_terminal::clipboard::Clipboard::default();
        clipboard.set(nmt_terminal::clipboard::ClipboardType::Clipboard, text);
        self.clear_selection();
        true
    }

    fn paste(&self) -> bool {
        let mut clipboard = nmt_terminal::clipboard::Clipboard::default();
        let text = clipboard.get(nmt_terminal::clipboard::ClipboardType::Clipboard);
        let Some(bytes) = paste_payload(&text, self.modes().contains(Mode::BRACKETED_PASTE)) else {
            return false;
        };
        self.write_bytes(&bytes);
        true
    }

    fn modes(&self) -> Mode {
        let bits = self.session.vt_modes.load(Ordering::Relaxed);
        Mode::from_bits_truncate(bits)
    }

    fn mouse_mode(&self) -> Option<Mode> {
        let mode = self.modes();
        mode.intersects(Mode::MOUSE_MODE).then_some(mode)
    }

    fn app_mouse_mode(&self, modifiers: ModifiersState) -> Option<Mode> {
        if modifiers.shift_key() {
            return None;
        }
        self.mouse_mode()
    }

    fn report_mouse(
        &self,
        mode: Mode,
        button: u8,
        pressed: bool,
        col: u16,
        row: u16,
        modifiers: ModifiersState,
    ) -> bool {
        let Some(msg) = nmt_input::encode_mouse_report(
            mode.contains(Mode::SGR_MOUSE),
            button,
            mouse_report_mods(modifiers),
            pressed,
            col,
            row,
        ) else {
            return false;
        };
        self.write_bytes(&msg);
        true
    }
}

fn selection_screen_range(
    selection: &Selection,
    buf: &RenderBuffer,
    viewport_top: i32,
) -> Option<SelectionRange> {
    let mut range = selection.to_range_engine(buf, viewport_top, WORD_DELIMITERS)?;
    range.start.row += viewport_top;
    range.end.row += viewport_top;
    Some(range)
}

fn block_selection_range(
    block: &nmt_terminal::ghostty::BlockRef,
    palette: &nmt_terminal::ghostty::Palette,
    line: usize,
    col: u32,
    selection_type: SelectionType,
) -> Option<((usize, u32), (usize, u32))> {
    let cols = usize::from(block.cols());
    if cols == 0 || line >= block.row_count() {
        return None;
    }

    let wrapped = |row| {
        block
            .read_row_visit(row, palette, |_, _, _, _| {})
            .ok()
            .flatten()
            .map(|meta| meta.wrapped)
    };
    let mut first = line;
    while first > 0 && wrapped(first - 1)? {
        first -= 1;
    }
    let mut last = line;
    while last + 1 < block.row_count() && wrapped(last)? {
        last += 1;
    }

    if selection_type == SelectionType::Lines {
        return Some(((first, 0), (last, cols.saturating_sub(1) as u32)));
    }
    if selection_type != SelectionType::Semantic {
        let col = col.min(cols.saturating_sub(1) as u32);
        return Some(((line, col), (line, col)));
    }

    // Class 0 = whitespace, 1 = punctuation delimiter, 2 = word content.
    // Expanding one class matches terminal double-click behavior for words,
    // delimiter runs, and blank runs while retaining cell-accurate wide text.
    let mut classes = vec![0u8; (last - first + 1) * cols];
    for row in first..=last {
        let offset = (row - first) * cols;
        block
            .read_row_visit(row, palette, |x, text, wide, _| {
                use nmt_terminal::ghostty::CellWide;
                if matches!(wide, CellWide::SpacerHead | CellWide::SpacerTail) {
                    return;
                }
                let ch = text.as_str().chars().next().unwrap_or(' ');
                let class = if ch.is_whitespace() {
                    0
                } else if WORD_DELIMITERS.contains(ch) {
                    1
                } else {
                    2
                };
                let x = usize::from(x);
                if x < cols {
                    classes[offset + x] = class;
                    if wide == CellWide::Wide && x + 1 < cols {
                        classes[offset + x + 1] = class;
                    }
                }
            })
            .ok()
            .flatten()?;
    }

    let clicked = (line - first) * cols + (col as usize).min(cols - 1);
    let class = classes[clicked];
    let mut start = clicked;
    while start > 0 && classes[start - 1] == class {
        start -= 1;
    }
    let mut end = clicked;
    while end + 1 < classes.len() && classes[end + 1] == class {
        end += 1;
    }
    Some((
        (first + start / cols, (start % cols) as u32),
        (first + end / cols, (end % cols) as u32),
    ))
}

fn tab_state_with_cwd(launch: &TabState, last_cwd: Option<String>) -> TabState {
    let mut state = launch.clone();
    if last_cwd.is_some() {
        state.cwd = last_cwd;
    }
    state
}

/// OSC 7 reports the cwd as a `file://host/path` URI (or a plain path from
/// some shells); consumers (git status, session restore) need a filesystem
/// path. `file:///C:/x` → `C:/x`, `file://host/home/u` → `/home/u`.
fn normalize_osc7_pwd(pwd: &str) -> String {
    let Some(rest) = pwd.strip_prefix("file://") else {
        return pwd.to_string();
    };
    let Some(slash) = rest.find('/') else {
        return pwd.to_string();
    };
    let path = &rest[slash..];
    // "/C:/x" → "C:/x": Windows drive paths carry no leading slash.
    if path.as_bytes().get(2) == Some(&b':') {
        path[1..].to_string()
    } else {
        path.to_string()
    }
}

/// Place one sparse row cell into the column-aligned char buffer: gaps
/// (skipped blank cells) become spaces, and only the cell's first codepoint
/// is kept so char index stays == grid column.
fn push_pointer_cell(chars: &mut Vec<char>, x: u16, text: &str) {
    let x = x as usize;
    if chars.len() < x {
        chars.resize(x, ' ');
    }
    if chars.len() == x {
        chars.push(text.chars().next().unwrap_or(' '));
    }
}

fn pointer_row(
    mut chars: Vec<char>,
    cols: usize,
    meta: nmt_terminal::ghostty::ScreenRowMeta,
) -> PointerRow {
    // Pad to the full grid width so joined soft-wrapped rows keep every
    // segment exactly `cols` chars (column math stays trivial), and so a
    // blank tail reads as spaces that correctly terminate a URL token.
    if chars.len() < cols {
        chars.resize(cols, ' ');
    }
    PointerRow {
        text: chars.into_iter().collect(),
        wrapped: meta.wrapped,
        hyperlinks: meta.hyperlinks,
    }
}

fn paste_payload(text: &str, bracketed: bool) -> Option<Vec<u8>> {
    if text.is_empty() {
        return None;
    }
    let mut body = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        body = body.replace("\x1b[201~", "");
    }
    Some(nmt_input::bracket_paste(body.as_bytes(), bracketed))
}

fn mouse_button_code(button: SurfaceMouseButton) -> Option<u8> {
    match button {
        SurfaceMouseButton::Left => Some(0),
        SurfaceMouseButton::Middle => Some(1),
        SurfaceMouseButton::Right => Some(2),
    }
}

fn mouse_report_mods(modifiers: ModifiersState) -> u8 {
    let mut mods = 0;
    if modifiers.shift_key() {
        mods += 4;
    }
    if modifiers.alt_key() {
        mods += 8;
    }
    if modifiers.control_key() {
        mods += 16;
    }
    mods
}

fn should_scroll_to_bottom_before_input(offset: u64, total: u64, len: u64) -> bool {
    offset < total.saturating_sub(len)
}

#[cfg(test)]
mod tests {
    use nmt_input::keyboard::ModifiersState;
    use nmt_terminal::ghostty::GhosttyTerminal;
    use nmt_terminal::render_buffer::RenderBuffer;
    use nmt_terminal::selection::{Selection, SelectionType};
    use nmt_terminal::terminal::pos::{Column, Line, Pos, Side};

    use super::{
        SurfaceMouseButton, TerminalSurface, block_selection_range, mouse_button_code,
        mouse_report_mods, paste_payload, selection_screen_range,
        should_scroll_to_bottom_before_input, tab_state_with_cwd,
    };
    use crate::terminal::session::TerminalSessionConfig;

    #[test]
    fn bad_shell_returns_error() {
        let config = TerminalSessionConfig {
            shell: Some("this-shell-does-not-exist-xyz.exe".to_string()),
            ..TerminalSessionConfig::default()
        };

        let err = TerminalSurface::new(config, 1, None)
            .err()
            .expect("bad shell must fail");

        assert!(err.contains("PtySpawn"));
    }

    #[test]
    fn frozen_click_selection_expands_words_and_wrapped_lines() {
        let mut terminal = GhosttyTerminal::new(8, 4, 100).unwrap();
        terminal.write_vt(b"foo bar\r\nabcdefghijk");
        let handle = terminal.finish_block().unwrap().expect("block created");
        let block = terminal.block_acquire(handle).expect("block acquired");
        let palette = terminal.color_palette();

        assert_eq!(
            block_selection_range(&block, &palette, 0, 5, SelectionType::Semantic),
            Some(((0, 4), (0, 6)))
        );
        assert_eq!(
            block_selection_range(&block, &palette, 2, 1, SelectionType::Semantic),
            Some(((1, 0), (2, 2)))
        );
        assert_eq!(
            block_selection_range(&block, &palette, 2, 1, SelectionType::Lines),
            Some(((1, 0), (2, 7)))
        );
    }

    #[test]
    fn screen_word_selection_searches_the_visible_row_before_rebasing() {
        let mut terminal = GhosttyTerminal::new(24, 2, 100).unwrap();
        terminal.write_vt(b"pipelines.universal\r\npi");
        let mut buf = RenderBuffer::new(24, 2);
        terminal.snapshot_into(&mut buf).unwrap();
        let selection = Selection::new(
            SelectionType::Semantic,
            Pos::new(Line(1), Column(1)),
            Side::Left,
        );

        let range = selection_screen_range(&selection, &buf, 1).unwrap();

        assert_eq!(range.start, Pos::new(Line(1), Column(0)));
        assert_eq!(range.end, Pos::new(Line(1), Column(8)));
    }

    #[test]
    fn osc7_pwd_normalizes_to_filesystem_path() {
        use super::normalize_osc7_pwd;
        assert_eq!(
            normalize_osc7_pwd("file:///C:/Projects/example"),
            "C:/Projects/example"
        );
        assert_eq!(normalize_osc7_pwd("file://host/home/u"), "/home/u");
        assert_eq!(normalize_osc7_pwd(r"C:\plain\path"), r"C:\plain\path");
        assert_eq!(normalize_osc7_pwd("/unix/path"), "/unix/path");
    }

    #[test]
    fn tab_state_uses_last_reported_cwd() {
        let launch = nmt_config::local_state::TabState {
            name: None,
            user_named: false,
            shell: Some("pwsh.exe".into()),
            args: vec!["-NoLogo".into()],
            cwd: Some("C:/old".into()),
            panes: None,
        };

        let state = tab_state_with_cwd(&launch, Some("C:/new".into()));

        assert_eq!(state.shell, launch.shell);
        assert_eq!(state.args, launch.args);
        assert_eq!(state.cwd.as_deref(), Some("C:/new"));
    }

    #[test]
    fn paste_payload_normalizes_and_guards() {
        assert_eq!(paste_payload("", false), None);
        assert_eq!(
            paste_payload("a\r\nb\nc", false).unwrap(),
            b"a\rb\rc".to_vec()
        );
        assert_eq!(
            paste_payload("ls\x1b[201~rm", true).unwrap(),
            b"\x1b[200~lsrm\x1b[201~".to_vec()
        );
    }

    #[test]
    fn mouse_helpers_match_xterm_codes() {
        assert_eq!(mouse_button_code(SurfaceMouseButton::Left), Some(0));
        assert_eq!(mouse_button_code(SurfaceMouseButton::Middle), Some(1));
        assert_eq!(mouse_button_code(SurfaceMouseButton::Right), Some(2));
        assert_eq!(
            mouse_report_mods(ModifiersState::SHIFT | ModifiersState::CONTROL),
            20
        );
        assert_eq!(mouse_report_mods(ModifiersState::ALT), 8);
    }

    #[test]
    fn input_snap_only_when_viewport_is_scrolled_up() {
        assert!(should_scroll_to_bottom_before_input(3, 20, 10));
        assert!(!should_scroll_to_bottom_before_input(10, 20, 10));
        assert!(!should_scroll_to_bottom_before_input(0, 10, 20));
    }
}
