use std::sync::atomic::{AtomicBool, Ordering};
use std::{collections, time};

use nmt_config::CursorShape;
use nmt_config::colors::Colors;
use nmt_config::local_state::TabState;
use nmt_input::keyboard::ModifiersState;
use nmt_terminal::clipboard::{Clipboard, ClipboardType};
use nmt_terminal::render_buffer::RenderBuffer;
use nmt_terminal::selection::Selection;
use nmt_terminal::terminal::pos::{Line, Pos};
use parking_lot::Mutex;
use tracing::{trace, warn};

use crate::terminal;
use crate::terminal::frame::TerminalFrame;
use crate::terminal::metrics;
use crate::terminal::session::{HostEvent, TerminalSession, TerminalSessionConfig};
use crate::terminal::wake::{Wake, WakeSender, WakeSignal};

mod input;
mod mouse;
mod reads;
mod scroll;
mod selection;

#[cfg(test)]
use input::paste_payload;
pub(crate) use input::{TerminalKeyAction, TerminalKeyResult};
pub(crate) use mouse::{
    SurfaceCell, SurfaceCellSide, SurfaceMouseButton, SurfaceMouseEventKind, SurfaceScreenCell,
};
#[cfg(test)]
use mouse::{mouse_button_code, mouse_motion_code, mouse_report_mods};
#[cfg(test)]
use selection::{block_selection_range, selection_screen_range};

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

    pub(crate) fn set_theme_colors(&self, colors: &Colors) {
        self.session.engine.lock().set_theme_colors(colors);
    }

    pub(crate) fn set_cursor_shape(&self, shape: CursorShape) -> bool {
        let next = {
            let mut engine = self.session.engine.lock();

            if let Err(error) = engine.set_default_cursor_shape(shape) {
                warn!("failed to update cursor shape: {error}");
                return false;
            }

            engine.snapshot()
        };

        let next = match next {
            Ok(next) => next,
            Err(error) => {
                warn!("failed to refresh terminal after cursor shape change: {error}");
                return false;
            }
        };

        *self.session.render_buffer.lock() = next;

        true
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
        cursor_shape: CursorShape,
        environment_overrides: Vec<(String, String)>,
    ) -> Result<Self, String> {
        let wake_sender = WakeSender::from_fn(move |kind: Wake| {
            wake.signal(kind);
        });

        let config = TerminalSessionConfig {
            shell: shell.clone(),
            args: args.clone(),
            working_dir: working_dir.clone(),
            starting_title: Some(starting_title),
            cols: metrics::COLS,
            rows: metrics::ROWS,
            cursor_shape,
            environment_overrides,
            ..TerminalSessionConfig::default()
        };

        Self::new(config, surface_id, Some(wake_sender))
    }

    /// Build a GPUI surface backed by a remote session. The attach snapshot
    /// sizes the initial grid; live output/input/resize flow over the network.
    #[cfg(windows)]
    pub(crate) fn for_gpui_remote(
        wake: WakeSignal,
        surface_id: u64,
        remote: nmt_remote_net::RemoteSession,
    ) -> Result<Self, String> {
        let wake_sender = WakeSender::from_fn(move |kind: Wake| {
            wake.signal(kind);
        });

        let grid_size = (remote.snapshot().cols, remote.snapshot().rows);
        let session = TerminalSession::new_remote(remote, surface_id, Some(wake_sender))
            .map_err(|error| format!("{:?}: {}", error.code, error))?;

        Ok(Self {
            session,
            launch_state: TabState::default(),
            last_cwd: Mutex::new(None),
            selection: Mutex::new(None),
            read_only: AtomicBool::new(false),
            alt_screen: AtomicBool::new(false),
            grid_size,
        })
    }

    /// Number of processes beyond the shell itself in the shell's Job
    /// Object (requires the job-management setting; 0 otherwise).
    pub(crate) fn child_process_count(&self) -> usize {
        self.session.child_process_count()
    }

    pub fn poll_events(&self) -> Vec<HostEvent> {
        self.session.poll_events()
    }

    pub(crate) fn in_flight_block(&self) -> Option<terminal::session::InFlightBlock> {
        self.session.in_flight_block()
    }

    pub(crate) fn open_prompt_region(&self) -> bool {
        self.session.open_prompt_region()
    }

    pub(crate) fn mouse_reporting_active(&self) -> bool {
        self.mouse_mode().is_some()
    }

    pub(crate) fn mouse_reporting_active_for(&self, modifiers: ModifiersState) -> bool {
        self.app_mouse_mode(modifiers).is_some()
    }

    pub(crate) fn copy_text_to_clipboard(&self, text: String) {
        if text.is_empty() {
            return;
        }

        let mut clipboard = Clipboard::default();

        clipboard.set(ClipboardType::Clipboard, text);
    }

    pub(crate) fn set_last_cwd(&self, cwd: String) {
        *self.last_cwd.lock() = Some(normalize_osc7_pwd(&cwd));
    }

    pub(crate) fn tab_state(&self) -> TabState {
        tab_state_with_cwd(&self.launch_state, self.last_cwd.lock().clone())
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
        let total_start = time::Instant::now();
        let selection = self.selection_range();

        // Resolve live image generations before taking the render-buffer lock so the
        // generation-store and render locks are never nested. A graphics-free
        // session skips the store entirely via the lock-free live-image check, so it
        // pays nothing here.
        let generations = if self.session.has_live_images() {
            self.session.generation_store().lock().live_generations()
        } else {
            collections::HashMap::new()
        };

        let sel_us = total_start.elapsed().as_micros();

        let frame = self.with_render_buffer(|buf| {
            // Time spent here is *after* the render_buffer lock is acquired, so
            // (total - sel - extract) is the lock-wait + selection lock cost.
            let extract_start = time::Instant::now();
            let frame =
                TerminalFrame::from_render_buffer_reusing(buf, selection, &generations, previous);
            let extract_us = extract_start.elapsed().as_micros();

            trace!(
                target: "perf",
                rows = buf.rows(),
                cols = buf.cols(),
                extract_us,
                "frame extract (inside render_buffer lock)"
            );

            frame
        });

        trace!(
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

    /// SCREEN row of the top visible row (0 when the viewport is empty).
    pub(super) fn viewport_top(&self) -> i32 {
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

    /// Re-base a viewport-relative cell onto SCREEN coordinates, so the anchor
    /// stays on the same content when the viewport scrolls.
    pub(super) fn screen_pos(&self, pos: Pos) -> Pos {
        Pos::new(Line(pos.row.0 + self.viewport_top()), pos.col)
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

/// Place one sparse row cell into the column-aligned char buffer: gaps
/// (skipped blank cells) become spaces, and only the cell's first codepoint
/// is kept so char index stays == grid column.
#[cfg(test)]
mod tests;
