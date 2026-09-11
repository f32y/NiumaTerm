use std::sync::Arc;

use crate::ghostty::{BlockHandle, ScreenRowMeta, ScreenRowRead};
use crate::graphics::GraphicData;
use crate::render_buffer::RenderBuffer;
use crate::session::TerminalSession;
use crate::session::page::{PageSource, RowPage};

/// Pointer text preserves grid columns by retaining the first codepoint per cell.
#[derive(Clone, Debug)]
pub struct RowText {
    pub text: String,
    pub wrapped: bool,
    pub hyperlinks: Vec<(u16, u16, String)>,
}

impl TerminalSession {
    pub fn take_block_image(&self, handle: BlockHandle, image_id: u32) -> Option<GraphicData> {
        self.pages
            .lock()
            .take_image(handle, image_id, &self.messenger)
    }

    pub fn screen_page(&self, row: usize) -> Option<Arc<RowPage>> {
        self.screen_page_at(self.snapshot().revision, row)
    }

    pub fn screen_page_at(&self, revision: u64, row: usize) -> Option<Arc<RowPage>> {
        self.pages
            .lock()
            .read(PageSource::Screen { revision }, row, &self.messenger)
    }

    pub fn block_page(&self, handle: BlockHandle, row: usize) -> Option<Arc<RowPage>> {
        let source = PageSource::Block {
            id: handle.id,
            generation: handle.generation,
            theme: self.snapshot().theme_revision,
        };
        self.pages.lock().read(source, row, &self.messenger)
    }

    pub fn screen_row_text(&self, row: u32) -> Option<RowText> {
        self.screen_row_text_in(&self.snapshot(), row)
    }

    pub fn screen_row_text_in(&self, snapshot: &RenderBuffer, row: u32) -> Option<RowText> {
        if let Some(index) = snapshot.viewport_top.and_then(|top| row.checked_sub(top))
            && let Some(cells) = snapshot.grid().get(index as usize)
        {
            return Some(RowText {
                text: cells
                    .inner
                    .iter()
                    .map(|cell| match cell.c() {
                        '\0' => ' ',
                        c => c,
                    })
                    .collect(),
                wrapped: snapshot.row_wrapped(index as usize),
                hyperlinks: snapshot.row_hyperlinks(index as usize).to_vec(),
            });
        }
        let page = self.screen_page_at(snapshot.revision, row as usize)?;
        Some(materialized_pointer_row(page.row(row as usize)?, page.cols))
    }

    pub fn block_row_text(&self, item: usize, row: usize) -> Option<RowText> {
        let page = self.block_page(self.block_handle(item)?, row)?;
        Some(materialized_pointer_row(page.row(row)?, page.cols))
    }
}

fn materialized_pointer_row(row: &ScreenRowRead, cols: u16) -> RowText {
    let mut chars = Vec::with_capacity(cols as usize);
    for cell in &row.cells {
        push_pointer_cell(&mut chars, cell.x, cell.text.as_str());
    }
    pointer_row(
        chars,
        cols as usize,
        ScreenRowMeta {
            wrapped: row.wrapped,
            hyperlinks: row.hyperlinks.clone(),
            ..ScreenRowMeta::default()
        },
    )
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

fn pointer_row(mut chars: Vec<char>, cols: usize, meta: ScreenRowMeta) -> RowText {
    if chars.len() < cols {
        chars.resize(cols, ' ');
    }
    RowText {
        text: chars.into_iter().collect(),
        wrapped: meta.wrapped,
        hyperlinks: meta.hyperlinks,
    }
}
