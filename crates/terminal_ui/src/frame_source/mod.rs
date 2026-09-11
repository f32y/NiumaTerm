use std::sync::Arc;
use std::{collections, time};

use nmt_config::colors::Colors;
use nmt_terminal::render_buffer::RenderBuffer;
use nmt_terminal::session::{EngineError, SessionObserver, TerminalSession, TerminalSessionConfig};
use tracing::trace;

use crate::frame::TerminalFrame;
use crate::metrics;
use crate::pane_model::FrameTheme;
use crate::session_bridge::SessionBridge;
use crate::wake::{Wake, WakeSender, WakeSignal};

#[cfg(all(test, windows, enable_profiling))]
mod profile_tests;
mod reads;
#[cfg(test)]
mod tests;

pub struct TerminalFrameSource {
    pub(crate) session: TerminalSession,
    pub(crate) images: Arc<SessionBridge>,
    pub(crate) snapshot: Arc<RenderBuffer>,
    grid_size: (u16, u16),
}

impl TerminalFrameSource {
    pub fn new(
        config: TerminalSessionConfig,
        id: u64,
        wake: Option<WakeSender>,
        colors: Colors,
    ) -> Result<Self, String> {
        let grid_size = (config.cols, config.rows);
        let images = Arc::new(SessionBridge::new(id, wake));
        let session = TerminalSession::new(&config, id, colors, Some(images.clone()))
            .map_err(|error| format!("{:?}: {}", error.code, error))?;

        Ok(Self {
            snapshot: session.snapshot(),
            session,
            images,
            grid_size,
        })
    }

    pub(crate) fn for_gpui(
        wake: WakeSignal,
        surface_id: u64,
        launch: TerminalSessionConfig,
        colors: Colors,
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

        Self::new(config, surface_id, Some(wake_sender), colors)
    }

    pub(crate) fn attach(
        wake: WakeSignal,
        id: u64,
        connect: impl FnOnce(Arc<dyn SessionObserver>) -> Result<TerminalSession, EngineError>,
    ) -> Result<Self, String> {
        let images = Arc::new(SessionBridge::new(
            id,
            Some(WakeSender::from_fn(move |kind| {
                wake.signal(kind);
            })),
        ));
        let session =
            connect(images.clone()).map_err(|error| format!("{:?}: {}", error.code, error))?;
        let grid_size =
            session.with_render_buffer(|buffer| (buffer.cols() as u16, buffer.rows() as u16));
        Ok(Self {
            snapshot: session.snapshot(),
            session,
            images,
            grid_size,
        })
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

    pub(crate) fn frame(
        &mut self,
        previous: Option<&TerminalFrame>,
        theme: &FrameTheme,
    ) -> TerminalFrame {
        let total_start = time::Instant::now();
        self.snapshot = self.session.snapshot();
        let snapshot = &self.snapshot;
        let selection = self.session.selection_range_in(snapshot);

        // Resolve image generations after retaining the frame. Graphics-free
        // sessions skip the image store entirely.
        let generations = if self.images.has_live_images() {
            self.images.generations.lock().live_generations()
        } else {
            collections::HashMap::new()
        };

        let sel_us = total_start.elapsed().as_micros();

        let frame = {
            let buf = snapshot;
            let extract_start = time::Instant::now();
            let frame = TerminalFrame::from_render_buffer_reusing(
                buf,
                selection,
                &generations,
                previous,
                theme,
            );
            let extract_us = extract_start.elapsed().as_micros();

            trace!(
                target: "perf",
                rows = buf.rows(),
                cols = buf.cols(),
                extract_us,
                "immutable frame extract"
            );

            frame
        };

        trace!(
            target: "perf",
            sel_us,
            total_us = total_start.elapsed().as_micros(),
            "frame total (snapshot + selection + extract)"
        );

        frame
    }
}
