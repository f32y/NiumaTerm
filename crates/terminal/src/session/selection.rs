#[cfg(test)]
#[path = "selection_tests.rs"]
mod selection_tests;

use parking_lot::Mutex;

use crate::ghostty::{BlockRef, Palette};
use crate::render_buffer::RenderBuffer;
use crate::selection::{Selection, SelectionRange, SelectionType, WORD_DELIMITERS};
use crate::session::mouse::{SurfaceCellSide, SurfaceMouseEventKind, SurfaceScreenCell};
use crate::terminal::pos::{Column, Line, Pos, Side};

/// The engine-region selection and the gestures that build it. Anchors are held
/// in SCREEN coordinates so a selection stays on the same content while the
/// viewport scrolls; every query therefore takes the caller's current
/// `viewport_top` instead of caching one that would go stale on the next scroll.
#[derive(Default)]
pub(super) struct SurfaceSelection {
    pub(super) selection: Mutex<Option<Selection>>,
}

impl SurfaceSelection {
    /// Apply a selection gesture to an absolute SCREEN cell. The block-list
    /// live history is rendered outside the engine viewport, but its rows keep
    /// these coordinates so selection and copy still use Ghostty's formatter.
    pub(super) fn apply_screen(
        &self,
        cell: SurfaceScreenCell,
        side: SurfaceCellSide,
        kind: SurfaceMouseEventKind,
        selection_type: SelectionType,
    ) -> bool {
        let Ok(row) = i32::try_from(cell.row) else {
            return false;
        };

        self.apply_at(
            Pos::new(Line(row), Column(cell.col as usize)),
            side,
            kind,
            selection_type,
        )
    }

    /// Drop the engine-region selection (block-split: a frozen-region
    /// selection replaces it, and vice versa).
    pub(super) fn clear(&self) {
        *self.selection.lock() = None;
    }

    pub(super) fn apply_at(
        &self,
        pos: Pos,
        side: SurfaceCellSide,
        kind: SurfaceMouseEventKind,
        selection_type: SelectionType,
    ) -> bool {
        let side = match side {
            SurfaceCellSide::Left => Side::Left,
            SurfaceCellSide::Right => Side::Right,
        };

        let mut guard = self.selection.lock();

        match kind {
            SurfaceMouseEventKind::Down => {
                let had_selection = guard.is_some();

                *guard = Some(Selection::new(selection_type, pos, side));

                had_selection || selection_type != SelectionType::Simple
            }

            SurfaceMouseEventKind::Move => {
                let Some(selection) = guard.as_mut() else {
                    return false;
                };

                selection.update(pos, side);

                true
            }

            SurfaceMouseEventKind::Up => {
                if guard.as_ref().is_some_and(Selection::is_empty) {
                    *guard = None;
                }

                false
            }
        }
    }
}

pub(super) fn selection_screen_range(
    selection: &Selection,
    buf: &RenderBuffer,
    viewport_top: i32,
) -> Option<SelectionRange> {
    let mut range = selection.to_range_engine(buf, viewport_top, WORD_DELIMITERS)?;

    range.start.row += viewport_top;
    range.end.row += viewport_top;

    Some(range)
}

pub(crate) fn block_selection_range(
    block: &BlockRef,
    palette: &Palette,
    line: usize,
    col: u32,
    selection_type: SelectionType,
) -> Option<((usize, u32), (usize, u32))> {
    let cols: usize = block.cols().into();

    if cols == 0 || line >= block.row_count() {
        return None;
    }

    let wrapped = |row| {
        block
            .read_row_visit(row, palette, |_, _, _, _| {})
            .ok()
            .flatten()
            .map(|meta| meta.wrapped)
    };

    let mut first = line;

    while first > 0 && wrapped(first - 1)? {
        first -= 1;
    }

    let mut last = line;

    while last + 1 < block.row_count() && wrapped(last)? {
        last += 1;
    }

    match selection_type {
        SelectionType::Lines => {
            return Some(((first, 0), (last, cols.saturating_sub(1) as u32)));
        }

        SelectionType::Simple | SelectionType::Block => {
            let col = col.min(cols.saturating_sub(1) as u32);

            return Some(((line, col), (line, col)));
        }

        SelectionType::Semantic => {}
    }

    // Class 0 = whitespace, 1 = punctuation delimiter, 2 = word content.
    // Expanding one class matches terminal double-click behavior for words,
    // delimiter runs, and blank runs while retaining cell-accurate wide text.
    let mut classes = vec![0u8; (last - first + 1) * cols];

    for row in first..=last {
        let offset = (row - first) * cols;

        block
            .read_row_visit(row, palette, |x, text, wide, _| {
                use crate::ghostty::CellWide;

                if matches!(wide, CellWide::SpacerHead | CellWide::SpacerTail) {
                    return;
                }

                let ch = text.as_str().chars().next().unwrap_or(' ');

                let class = if ch.is_whitespace() {
                    0
                } else if WORD_DELIMITERS.contains(ch) {
                    1
                } else {
                    2
                };

                let x: usize = x.into();

                if x < cols {
                    classes[offset + x] = class;

                    if wide == CellWide::Wide && x + 1 < cols {
                        classes[offset + x + 1] = class;
                    }
                }
            })
            .ok()
            .flatten()?;
    }

    let clicked = (line - first) * cols + (col as usize).min(cols - 1);
    let class = classes[clicked];

    let mut start = clicked;

    while start > 0 && classes[start - 1] == class {
        start -= 1;
    }

    let mut end = clicked;

    while end + 1 < classes.len() && classes[end + 1] == class {
        end += 1;
    }

    Some((
        (first + start / cols, (start % cols) as u32),
        (first + end / cols, (end % cols) as u32),
    ))
}
