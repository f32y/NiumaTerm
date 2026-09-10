use std::{ops, sync};

use nmt_terminal::ghostty::{BlockHandle, BlockRef, ScreenRowMeta};

use crate::surface::TerminalSurface;
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
    /// Read one absolute SCREEN row for pointer URL hit-testing.
    pub(crate) fn pointer_screen_row(&self, row: u32) -> Option<PointerRow> {
        self.session.with_screen_reader(|engine| {
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
        })
    }

    /// Read one row of a finished engine block for pointer URL hit-testing.
    pub(crate) fn pointer_block_row(&self, handle: BlockHandle, row: usize) -> Option<PointerRow> {
        self.session.with_screen_reader(|engine| {
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
        })
    }

    /// The cached frozen generation for `(block_id, image_id)`, if a paint
    /// already read it out of the engine block, avoiding eager image uploads.
    pub(crate) fn frozen_image(
        &self,
        block_id: u64,
        image_id: u32,
    ) -> Option<sync::Arc<graphics::ImageGeneration>> {
        self.images
            .frozen
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
        self.images
            .frozen
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
        let release = self.images.generations.lock().release_queue();

        let data = self.session.block_image_pixels(block, image_id)?;

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

        self.session.with_screen_reader(|engine| {
            let palette = engine.color_palette();
            let default_fg = frame::theme_default_foreground();

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
