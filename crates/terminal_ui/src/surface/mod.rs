use std::sync::Arc;
use std::{collections, time};

use nmt_config::local_state::TabState;
use nmt_config::{CursorShape, active_colors};
#[cfg(windows)]
use nmt_remote_net::{RemoteSession, net_pty::NetPty};
use nmt_terminal::clipboard::{Clipboard, ClipboardType};
#[cfg(windows)]
use nmt_terminal::pty_pipe::SessionOptions;
use nmt_terminal::session::{TerminalSession, TerminalSessionConfig};
use tracing::trace;

use crate::frame::TerminalFrame;
use crate::graphics::SessionImages;
use crate::metrics;
use crate::wake::{Wake, WakeSender, WakeSignal};

mod input;
#[cfg(all(test, windows))]
mod profile_tests;
mod reads;
#[cfg(test)]
mod tests;

pub(crate) use nmt_terminal::session::{
    SurfaceCell, SurfaceCellSide, SurfaceMouseButton, SurfaceMouseEventKind, SurfaceScreenCell,
};

pub(crate) use crate::surface::input::{TerminalKeyAction, TerminalKeyResult};

pub struct TerminalSurface {
    pub(crate) session: TerminalSession,
    pub(crate) images: Arc<SessionImages>,
    launch_state: TabState,
    grid_size: (u16, u16),
}

impl TerminalSurface {
    pub fn new(
        config: TerminalSessionConfig,
        id: u64,
        wake: Option<WakeSender>,
    ) -> Result<Self, String> {
        let grid_size = (config.cols, config.rows);
        let launch_state = restorable_tab_state(&config);
        let config = config.with_shell_integration();
        let images = Arc::new(SessionImages::new(id, wake));
        let session = TerminalSession::new(&config, id, active_colors(), Some(images.clone()))
            .map_err(|error| format!("{:?}: {}", error.code, error))?;

        Ok(Self {
            session,
            images,
            launch_state,
            grid_size,
        })
    }

    pub(crate) fn for_gpui(
        wake: WakeSignal,
        surface_id: u64,
        launch: TerminalSessionConfig,
    ) -> Result<Self, String> {
        let wake_sender = WakeSender::from_fn(move |kind: Wake| {
            wake.signal(kind);
        });

        // The initial grid is always the fixed metrics size; the real
        // dimensions arrive with the first layout pass, so a caller-supplied
        // size would only be overwritten.
        let config = TerminalSessionConfig {
            cols: metrics::COLS,
            rows: metrics::ROWS,
            ..launch
        };

        Self::new(config, surface_id, Some(wake_sender))
    }

    #[cfg(windows)]
    pub(crate) fn for_gpui_remote(
        wake: WakeSignal,
        surface_id: u64,
        remote: RemoteSession,
    ) -> Result<Self, String> {
        let wake_sender = WakeSender::from_fn(move |kind: Wake| {
            wake.signal(kind);
        });

        let grid_size = (remote.snapshot().cols, remote.snapshot().rows);
        let images = Arc::new(SessionImages::new(surface_id, Some(wake_sender)));
        let session = TerminalSession::from_pty(
            NetPty::new(remote),
            None,
            SessionOptions {
                cols: grid_size.0.max(1),
                rows: grid_size.1.max(1),
                route_id: surface_id as usize,
                colors: active_colors(),
                cursor_shape: CursorShape::Block,
                scrollback_lines: 10_000,
                engine_blocks: false,
                terminal_responses: true,
                output_sink: None,
            },
            Some(images.clone()),
        )
        .map_err(|error| format!("{:?}: {}", error.code, error))?;

        Ok(Self {
            session,
            images,
            launch_state: TabState::default(),
            grid_size,
        })
    }

    pub(crate) fn copy_text_to_clipboard(&self, text: String) {
        if text.is_empty() {
            return;
        }

        let mut clipboard = Clipboard::default();

        clipboard.set(ClipboardType::Clipboard, text);
    }

    pub(crate) fn tab_state(&self) -> TabState {
        tab_state_with_cwd(&self.launch_state, self.session.current_directory())
    }

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

        let accepted = self.session.resize(
            cols,
            rows,
            metrics::pixel_u16(width_px),
            metrics::pixel_u16(height_px),
        );

        if accepted {
            self.grid_size = (cols, rows);
        }
        accepted
    }

    pub(crate) fn frame(&self, previous: Option<&TerminalFrame>) -> TerminalFrame {
        let total_start = time::Instant::now();
        let selection = self.session.selection_range();

        // Resolve live image generations before taking the render-buffer lock so the
        // generation-store and render locks are never nested. A graphics-free
        // session skips the store entirely via the lock-free live-image check, so it
        // pays nothing here.
        let generations = if self.images.has_live_images() {
            self.images.generations.lock().live_generations()
        } else {
            collections::HashMap::new()
        };

        let sel_us = total_start.elapsed().as_micros();

        let frame = self.session.with_render_buffer(|buf| {
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
}

fn tab_state_with_cwd(launch: &TabState, last_cwd: Option<String>) -> TabState {
    let mut state = launch.clone();

    if last_cwd.is_some() {
        state.cwd = last_cwd;
    }

    state
}

pub(crate) fn restorable_tab_state(config: &TerminalSessionConfig) -> TabState {
    TabState {
        name: None,
        user_named: false,
        shell: config.shell.clone(),
        args: config.args.clone(),
        cwd: config.working_dir.clone(),
        agent: None,
        agent_profile: None,
        panes: None,
    }
}
