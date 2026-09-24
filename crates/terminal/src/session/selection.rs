#[cfg(test)]
#[path = "selection_tests.rs"]
mod selection_tests;

use parking_lot::Mutex;

use crate::block_store::BlockStore;
use crate::ghostty::{BlockHandle, BlockRef, CellWide, Palette};
use crate::grid::{Column, Line, Pos, Row, Side, Square, Wide};
use crate::render_buffer::RenderBuffer;
use crate::selection::{Selection, SelectionRange, SelectionType, VisibleGrid, WORD_DELIMITERS};
use crate::session::{SurfaceCellSide, SurfaceMouseEventKind, SurfaceScreenCell};

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

    if matches!(selection_type, SelectionType::Simple | SelectionType::Block) {
        let col = col.min(cols.saturating_sub(1) as u32);

        return Some(((line, col), (line, col)));
    }

    // The logical line is materialized into the shape the live screen's
    // selection reads, so a double or triple click in frozen history picks
    // the same word or line it would have picked while the text was live.
    let mut rows: Vec<Row<Square>> = Vec::with_capacity(last - first + 1);
    let mut wrapped = Vec::with_capacity(last - first + 1);

    for row in first..=last {
        let mut cells: Row<Square> = Row::new(cols);

        let meta = block
            .read_row_visit(row, palette, |x, text, wide, _| {
                let x: usize = x.into();

                if x >= cols {
                    return;
                }

                let square = &mut cells[Column(x)];

                square.set_c(text.as_str().chars().next().unwrap_or('\0'));

                square.set_wide(match wide {
                    CellWide::Narrow => Wide::Narrow,
                    CellWide::Wide => Wide::Wide,
                    CellWide::SpacerTail => Wide::Spacer,
                    CellWide::SpacerHead => Wide::LeadingSpacer,
                });
            })
            .ok()
            .flatten()?;

        rows.push(cells);
        wrapped.push(meta.wrapped);
    }

    let grid = VisibleGrid::new(&rows, cols, &wrapped);

    let point = Pos::new(
        Line((line - first) as i32),
        Column((col as usize).min(cols - 1)),
    );

    let range = Selection::expand_point(&grid, point, selection_type, WORD_DELIMITERS)?;

    let block_point = |pos: Pos| (first + pos.row.0 as usize, pos.col.0 as u32);

    Some((block_point(range.start), block_point(range.end)))
}

/// A position in the frozen history: store item, physical block row, column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlockPoint {
    pub item: usize,
    pub line: usize,
    pub col: u32,
}

/// One immutable request range, resolved by the engine owner.
#[derive(Debug)]
pub(super) struct FrozenSelectionPiece {
    pub handle: BlockHandle,

    /// `(row, col)` start within the block; `None` = the block's start.
    pub start: Option<(usize, u32)>,

    /// Inclusive `(row, col)` end within the block; `None` = the block's end.
    pub end: Option<(usize, u32)>,
}

/// The per-block ranges of the frozen selection (inclusive endpoints), in
/// item order. Join the formatted pieces with `\n`.
pub(super) fn frozen_selection_pieces(
    store: &BlockStore,
    a: BlockPoint,
    b: BlockPoint,
) -> Vec<FrozenSelectionPiece> {
    let (a, b) = if a <= b { (a, b) } else { (b, a) };

    let mut out = Vec::new();

    for (item_idx, item) in store.items().iter().enumerate() {
        if item_idx < a.item || item_idx > b.item {
            continue;
        }

        let Some(handle) = item.handle() else {
            continue;
        };

        out.push(FrozenSelectionPiece {
            handle,
            start: (item_idx == a.item).then_some((a.line, a.col)),
            end: (item_idx == b.item).then_some((b.line, b.col)),
        });
    }

    out
}
