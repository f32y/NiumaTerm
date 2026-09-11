use crate::ghostty::ScreenRowRead;

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
        let x = cell.x as usize;

        if chars.len() < x {
            chars.resize(x, ' ');
        }

        if chars.len() == x {
            chars.push(cell.text.as_str().chars().next().unwrap_or(' '));
        }
    }

    let cols = cols as usize;

    if chars.len() < cols {
        chars.resize(cols, ' ');
    }

    RowText {
        text: chars.into_iter().collect(),
        wrapped: row.wrapped,
        hyperlinks: row.hyperlinks.clone(),
    }
}
