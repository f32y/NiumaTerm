use std::sync::atomic::{AtomicBool, Ordering};

use nmt_config::local_state::TabState;
use nmt_input::keyboard::ModifiersState;
use nmt_terminal::render_buffer::RenderBuffer;
use nmt_terminal::selection::{Selection, SelectionRange, SelectionType};
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
        let side = match side {
            SurfaceCellSide::Left => Side::Left,
            SurfaceCellSide::Right => Side::Right,
        };
        match kind {
            SurfaceMouseEventKind::Down => self.begin_selection_cell(pos, side),
            SurfaceMouseEventKind::Move => self.update_selection_cell(pos, side),
            SurfaceMouseEventKind::Up => self.finish_selection(),
        }
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
        let Ok(mut snap) = snap else {
            return false;
        };
        let changed = snap.scrollbar != before;
        let mut buf = self.session.render_buffer.lock();
        snap.cursor.visible = buf.cursor_visible();
        buf.update(&snap);
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

    pub(crate) fn frame(&self) -> TerminalFrame {
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
                TerminalFrame::from_render_buffer_with_selection(buf, selection, &generations);
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
        let guard = self.selection.lock();
        let sel = guard.as_ref()?;
        let top = self.viewport_top();
        let buf = self.session.render_buffer.lock();
        sel.to_range_engine(&buf, top, "")
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

    fn begin_selection_cell(&self, pos: Pos, side: Side) -> bool {
        let pos = self.screen_pos(pos);
        let mut guard = self.selection.lock();
        let had_selection = guard.is_some();
        *guard = Some(Selection::new(SelectionType::Simple, pos, side));
        had_selection
    }

    fn update_selection_cell(&self, pos: Pos, side: Side) -> bool {
        let pos = self.screen_pos(pos);
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
        // viewport_top = 0 keeps the resolved range in SCREEN coordinates,
        // which is what `format_screen_range` consumes.
        let range = {
            let guard = self.selection.lock();
            let sel = guard.as_ref()?;
            let buf = self.session.render_buffer.lock();
            sel.to_range_engine(&buf, 0, "")?
        };
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

    use super::{
        SurfaceMouseButton, TerminalSurface, mouse_button_code, mouse_report_mods, paste_payload,
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
