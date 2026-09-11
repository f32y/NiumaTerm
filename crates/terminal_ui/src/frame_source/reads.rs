use std::{ops, sync};

use nmt_terminal::ghostty::BlockHandle;
use nmt_terminal::session::page::{PageSource, RowPage};

use crate::frame_source::TerminalFrameSource;
use crate::{block_list, frame, graphics};

impl TerminalFrameSource {
    pub(crate) fn frozen_image(
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

    pub(crate) fn live_history_lines(
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
            Some((row, builder.finish()))
        })
        .collect()
    }
}
