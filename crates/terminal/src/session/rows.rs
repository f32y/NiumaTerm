use crate::ghostty::ScreenRowMeta;
use crate::session::TerminalSession;

/// One row read for pointer URL hit-testing: plain text padded to the grid
/// width so char index == grid column (only each cell's first codepoint is
/// kept — grapheme extras would break the column mapping), the row's OSC 8
/// spans, and its soft-wrap flag.
pub struct RowText {
    pub text: String,
    pub wrapped: bool,
    /// OSC 8 spans: `(start_col, end_col_inclusive, uri)`.
    pub hyperlinks: Vec<(u16, u16, String)>,
}

impl TerminalSession {
    /// Read one absolute SCREEN row for pointer URL hit-testing.
    pub fn screen_row_text(&self, row: u32) -> Option<RowText> {
        self.with_screen_reader(|engine| {
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
    pub fn block_row_text(&self, item: usize, row: usize) -> Option<RowText> {
        let handle = self.block_handle(item)?;
        self.with_screen_reader(|engine| {
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
    // Pad to the full grid width so joined soft-wrapped rows keep every
    // segment exactly `cols` chars (column math stays trivial), and so a
    // blank tail reads as spaces that correctly terminate a URL token.
    if chars.len() < cols {
        chars.resize(cols, ' ');
    }

    RowText {
        text: chars.into_iter().collect(),
        wrapped: meta.wrapped,
        hyperlinks: meta.hyperlinks,
    }
}
