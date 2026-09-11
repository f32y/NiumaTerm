use crate::ghostty::{ScreenRowMeta, ScreenRowRead};

/// Pointer text preserves grid columns by retaining the first codepoint per cell.
#[derive(Clone, Debug)]
pub struct RowText {
    pub text: String,
    pub wrapped: bool,
    pub hyperlinks: Vec<(u16, u16, String)>,
}

pub(super) fn materialized_pointer_row(row: &ScreenRowRead, cols: u16) -> RowText {
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
