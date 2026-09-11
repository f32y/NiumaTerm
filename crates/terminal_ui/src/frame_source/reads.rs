use std::{ops, sync};

use nmt_terminal::ghostty::BlockRef;

use crate::frame_source::TerminalFrameSource;
use crate::{block_list, frame, graphics};

impl TerminalFrameSource {
    pub(crate) fn frozen_image(
        &self,
        block: &BlockRef,
        image_id: u32,
    ) -> Option<sync::Arc<graphics::ImageGeneration>> {
        let key = (block.handle().id, image_id);
        if let Some(generation) = self.images.frozen.lock().get(&key).cloned() {
            return Some(generation);
        }
        let release = self.images.generations.lock().release_queue();
        let data = self.session.block_image_pixels(block, image_id)?;
        let generation = graphics::graphic_to_generation(data, &release)?;
        self.images.frozen.lock().insert(key, generation.clone());
        Some(generation)
    }

    /// Engine-blocks mode: read active-grid scrollback rows (SCREEN
    /// coordinates) for the live item's scrolled-up history, replacing the harvested
    /// `Tail`. One engine lock hold covers the whole
    /// visible range; each row materializes as a display line.
    pub(crate) fn live_history_lines(
        &self,
        rows: ops::Range<u64>,
        default_fg: frame::TerminalColor,
    ) -> Vec<(u64, frame::TerminalLine)> {
        if rows.is_empty() {
            return Vec::new();
        }

        self.session.with_screen_reader(|engine| {
            let palette = engine.color_palette();

            rows.filter_map(|row| {
                let mut builder = block_list::EngineRowBuilder::default();

                engine
                    .read_screen_row_visit(
                        row.min(u32::MAX as u64) as u32,
                        &palette,
                        |x, t, w, s| builder.push(x, t, w, &s, default_fg),
                    )
                    .ok()
                    .flatten()
                    .map(|_| (row, builder.finish()))
            })
            .collect()
        })
    }
}
