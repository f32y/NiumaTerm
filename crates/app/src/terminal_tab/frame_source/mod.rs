pub(super) use crate::terminal_tab::frame_source::items::ItemViewport;

mod items;

#[cfg(test)]
#[cfg(all(test, windows, enable_profiling))]
mod profile_tests;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{collections, ops, sync, time};

use nmt_config::colors::Colors;
use nmt_terminal::clipboard::{Clipboard, ClipboardType};
use nmt_terminal::event::BlockEvent;
use nmt_terminal::ghostty::BlockHandle;
use nmt_terminal::graphics::UpdateQueues;
use nmt_terminal::render_buffer::RenderBuffer;
use nmt_terminal::session::page::{PAGE_ROWS, PageSource, RowPage};
use nmt_terminal::session::{
    BlockPoint, EngineError, SessionChange, SessionObserver, TerminalSession, TerminalSessionConfig,
};
use parking_lot::Mutex;
use tracing::trace;

use crate::terminal_tab::block_list::FrozenView;
use crate::terminal_tab::block_list::chrome::DurationLabels;
use crate::terminal_tab::frame::{TerminalColor, TerminalFrame};
use crate::terminal_tab::graphics::{FrozenImageCache, GenerationStore, prune_frozen_images};
use crate::terminal_tab::pane_model::FrameTheme;
use crate::terminal_tab::wake::{Wake, WakeSender, WakeSignal};
use crate::terminal_tab::{block_list, frame, graphics, metrics};

pub struct TerminalFrameSource {
    pub(super) session: TerminalSession,
    pub(super) images: Arc<SessionBridge>,
    pub(super) snapshot: Arc<RenderBuffer>,
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

    pub(super) fn for_gpui(
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

    pub(super) fn attach(
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

    pub(super) fn resize_for_content(
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

    pub(super) fn frame(
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

    pub(super) fn frozen_block_view(
        &self,
        item_idx: usize,
        viewport: &ItemViewport,
        selection: Option<(BlockPoint, BlockPoint)>,
        selected_item: Option<usize>,
        labels: &DurationLabels,
        foreground: TerminalColor,
    ) -> FrozenView {
        let Some((info, handle)) = self
            .session
            .block_item(item_idx)
            .and_then(|item| block_list::handle_item_info(&item, labels).zip(item.handle()))
        else {
            return FrozenView::default();
        };

        let visible = block_list::visible_rows(
            viewport.top,
            info.rows,
            viewport.height,
            viewport.cell_height,
            viewport.pad_rows,
        );

        let first = visible.start / PAGE_ROWS * PAGE_ROWS;

        let pages: Vec<_> = (first..visible.end)
            .step_by(PAGE_ROWS)
            .filter_map(|row| self.session.block_page(handle, row))
            .collect();

        let mut view = block_list::frozen_block_view(
            &pages,
            &info,
            item_idx,
            visible.clone(),
            viewport.cell_height,
            viewport.pad_rows,
            selection,
            selected_item,
            foreground,
        );

        let mut seen = HashSet::new();
        let mut placements = Vec::new();
        let mut generations = HashMap::new();

        for page in &pages {
            for placement in &page.placements {
                if seen.insert((
                    placement.image_id,
                    placement.placement_id,
                    placement.screen_col,
                    placement.screen_row,
                )) {
                    placements.push(*placement);
                }

                if let Some(generation) = self.frozen_image(page, placement.image_id) {
                    generations.insert(placement.image_id, generation);
                }
            }
        }

        view.images = block_list::frozen_block_images(
            &placements,
            &generations,
            &visible,
            viewport.cell_height,
            viewport.pad_rows,
        );

        view
    }

    pub(super) fn live_history_view(
        &self,
        history_rows: u64,
        cols: u32,
        viewport: &ItemViewport,
        foreground: TerminalColor,
    ) -> FrozenView {
        let visible = block_list::visible_rows(
            viewport.top,
            history_rows.min(usize::MAX as u64) as usize,
            viewport.height,
            viewport.cell_height,
            viewport.pad_rows,
        );

        let lines = self.live_history_lines(visible.start as u64..visible.end as u64, foreground);
        let selection = self.session.selection_screen_range_in(&self.snapshot);

        block_list::live_history_view(
            lines,
            history_rows,
            cols,
            viewport.cell_height,
            viewport.pad_rows,
            selection,
        )
    }

    pub(super) fn frozen_image(
        &self,
        page: &RowPage,
        image_id: u32,
    ) -> Option<sync::Arc<graphics::ImageGeneration>> {
        let PageSource::Block { id, generation, .. } = page.source else {
            return None;
        };

        let key = (id, image_id);

        if let Some(generation) = self.images.frozen.lock().get(&key).cloned() {
            return Some(generation);
        }

        let release = self.images.generations.lock().release_queue();

        let generation = graphics::graphic_to_generation(
            self.session
                .take_block_image(BlockHandle { id, generation }, image_id)?,
            &release,
        )?;

        self.images.frozen.lock().insert(key, generation.clone());

        Some(generation)
    }

    pub(super) fn live_history_lines(
        &self,
        rows: ops::Range<u64>,
        default_fg: frame::TerminalColor,
    ) -> Vec<(u64, frame::TerminalLine)> {
        rows.filter_map(|row| {
            let page = self
                .session
                .screen_page_at(self.snapshot.revision, usize::try_from(row).ok()?)?;

            let data = page.row(row as usize)?;
            let mut builder = block_list::EngineRowBuilder::default();

            for cell in &data.cells {
                builder.push(
                    cell.x,
                    cell.text.clone(),
                    cell.wide,
                    &cell.style,
                    default_fg,
                );
            }

            Some((row, builder.into()))
        })
        .collect()
    }
}

pub(super) struct SessionBridge {
    pub(super) generations: Mutex<GenerationStore>,
    pub(super) frozen: FrozenImageCache,
    live_count: AtomicUsize,
    id: u64,
    wake: Option<WakeSender>,
}

impl SessionBridge {
    pub(super) fn new(id: u64, wake: Option<WakeSender>) -> Self {
        Self {
            generations: Mutex::new(GenerationStore::default()),
            frozen: Arc::default(),
            live_count: AtomicUsize::new(0),
            id,
            wake,
        }
    }

    pub(super) fn has_live_images(&self) -> bool {
        self.live_count.load(Ordering::Relaxed) != 0
    }
}

impl SessionObserver for SessionBridge {
    fn graphics(&self, updates: UpdateQueues) {
        let mut store = self.generations.lock();

        for (id, data) in updates.pending_images {
            store.install(id, data);
        }

        for id in updates.remove_queue {
            store.remove(id.0 as u32);
        }

        self.live_count.store(store.len(), Ordering::Relaxed);
    }

    fn blocks(&self, events: &[BlockEvent]) {
        prune_frozen_images(&self.frozen, events);
    }

    fn clipboard(&self, kind: ClipboardType, text: String) {
        Clipboard::default().set(kind, text);
    }

    fn changed(&self, change: SessionChange) {
        if let Some(wake) = &self.wake {
            wake.send(match change {
                SessionChange::Content => Wake::Content(self.id),
                SessionChange::HostEvents => Wake::Chrome(self.id),
            });
        }
    }
}
