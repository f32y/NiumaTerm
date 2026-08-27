use std::{ops, sync};

use gpui::RenderImage;
use nmt_terminal::block_store::BlockStore;
use nmt_terminal::ghostty::{AcquiredBlock, BlockHandle, BlockRef, ScreenRowMeta};
use parking_lot::Mutex;

use super::TerminalSurface;
use crate::{block_list, frame, graphics};

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

impl TerminalSurface {
    /// Shared frozen block-split history (renderer read side).
    pub(crate) fn block_store(&self) -> sync::Arc<Mutex<BlockStore>> {
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
    pub(crate) fn acquire_block(&self, handle: BlockHandle) -> Option<AcquiredBlock> {
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
    pub(crate) fn pointer_block_row(&self, handle: BlockHandle, row: usize) -> Option<PointerRow> {
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
    ) -> Option<sync::Arc<graphics::ImageGeneration>> {
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
        generation: sync::Arc<graphics::ImageGeneration>,
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
        block: &BlockRef,
        image_id: u32,
    ) -> Option<sync::Arc<graphics::ImageGeneration>> {
        let release = self.session.generation_store().lock().release_queue();

        let data = {
            let engine = self.session.engine.lock();
            engine.block_image_pixels(block, image_id)?
        };

        graphics::graphic_to_generation(data, &release)
    }

    /// Engine-blocks mode: read active-grid scrollback rows (SCREEN
    /// coordinates) for the live item's scrolled-up history, replacing the harvested
    /// `Tail`. One engine lock hold covers the whole
    /// visible range; each row materializes as a display line.
    pub(crate) fn live_history_lines(
        &self,
        rows: ops::Range<u64>,
    ) -> Vec<(u64, frame::TerminalLine)> {
        if rows.is_empty() {
            return Vec::new();
        }

        let engine = self.session.engine.lock();
        let palette = engine.color_palette();
        let default_fg = frame::theme_default_foreground();

        rows.filter_map(|row| {
            let mut builder = block_list::EngineRowBuilder::default();
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
    pub(crate) fn drain_released_images(&self) -> Vec<sync::Arc<RenderImage>> {
        self.session.generation_store().lock().drain_released()
    }
}

fn push_pointer_cell(chars: &mut Vec<char>, x: u16, text: &str) {
    let x = x as usize;

    if chars.len() < x {
        chars.resize(x, ' ');
    }

    if chars.len() == x {
        chars.push(text.chars().next().unwrap_or(' '));
    }
}

fn pointer_row(mut chars: Vec<char>, cols: usize, meta: ScreenRowMeta) -> PointerRow {
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
