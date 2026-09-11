use crate::frame::TerminalFrame;
use crate::metrics::CellMetrics;
use crate::pane_model::PaneController;

impl PaneController {
    pub(crate) fn cell_metrics_or_measure(
        &mut self,
        measure: impl FnOnce() -> CellMetrics,
    ) -> CellMetrics {
        *self.cell_metrics.get_or_insert_with(measure)
    }

    pub(crate) fn resize_content(&mut self, width: f32, height: f32, cell: CellMetrics) -> bool {
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
    pub(crate) fn invalidate(&mut self) -> bool {
        self.frame_cache.invalidate();
        self.dirty.mark()
    }

    pub(crate) fn begin_frame(&mut self) -> TerminalFrame {
        self.dirty.begin_frame();
        if self.frame_cache.needs_rebuild() {
            self.refresh_frame();
        }
        self.frame_cache.current().unwrap_or_default()
    }

    pub(crate) fn begin_block_list_frame(&mut self) {
        self.frozen.begin_frame(self.block_list.active_top);
        self.update_viewport();
    }
}
