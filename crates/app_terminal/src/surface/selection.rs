use nmt_terminal::clipboard::{Clipboard, ClipboardType};
use nmt_terminal::ghostty::{BlockHandle, BlockRef, Palette};
use nmt_terminal::render_buffer::RenderBuffer;
use nmt_terminal::selection::{Selection, SelectionRange, SelectionType, WORD_DELIMITERS};
use nmt_terminal::terminal::pos::{Column, Line, Pos, Side};

use crate::surface::TerminalSurface;
use crate::surface::mouse::{SurfaceCellSide, SurfaceMouseEventKind, SurfaceScreenCell};

impl TerminalSurface {
    /// Apply a selection gesture to an absolute SCREEN cell. The block-list
    /// live history is rendered outside the engine viewport, but its rows keep
    /// these coordinates so selection and copy still use Ghostty's formatter.
    pub(crate) fn apply_screen_selection(
        &self,
        cell: SurfaceScreenCell,
        side: SurfaceCellSide,
        kind: SurfaceMouseEventKind,
        selection_type: SelectionType,
    ) -> bool {
        let Ok(row) = i32::try_from(cell.row) else {
            return false;
        };

        self.apply_selection_at(
            Pos::new(Line(row), Column(cell.col as usize)),
            side,
            kind,
            selection_type,
        )
    }

    pub(crate) fn frozen_selection_range(
        &self,
        handle: BlockHandle,
        line: usize,
        col: u32,
        selection_type: SelectionType,
    ) -> Option<((usize, u32), (usize, u32))> {
        let engine = self.session.engine.lock();
        let palette = engine.color_palette();
        let block = engine.block_acquire(handle)?;

        block_selection_range(&block, &palette, line, col, selection_type)
    }

    /// The selection mapped to visible-row coordinates for the frame highlight.
    /// Anchors are SCREEN coordinates (content-stable across scrolling), so the
    /// current `viewport_top` re-bases them; rows outside the viewport are
    /// clipped per-row by `row_selection_for`.
    pub(crate) fn selection_range(&self) -> Option<SelectionRange> {
        self.selection_range_at(self.viewport_top())
    }

    /// Selection in absolute SCREEN coordinates for live-history rows rendered
    /// above the pinned engine viewport.
    pub(crate) fn selection_screen_range(&self) -> Option<SelectionRange> {
        let viewport_top = self.viewport_top();
        let guard = self.selection.lock();
        let selection = guard.as_ref()?;
        let buf = self.session.render_buffer.lock();

        selection_screen_range(selection, &buf, viewport_top)
    }

    fn selection_range_at(&self, viewport_top: i32) -> Option<SelectionRange> {
        let guard = self.selection.lock();
        let sel = guard.as_ref()?;
        let buf = self.session.render_buffer.lock();

        sel.to_range_engine(&buf, viewport_top, WORD_DELIMITERS)
    }

    /// Drop the engine-region selection (block-split: a frozen-region
    /// selection replaces it, and vice versa).
    pub(crate) fn clear_selection(&self) {
        *self.selection.lock() = None;
    }

    pub(super) fn apply_selection_at(
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

        match kind {
            SurfaceMouseEventKind::Down => self.begin_selection(pos, side, selection_type),
            SurfaceMouseEventKind::Move => self.update_selection(pos, side),
            SurfaceMouseEventKind::Up => self.finish_selection(),
        }
    }

    fn begin_selection(&self, pos: Pos, side: Side, selection_type: SelectionType) -> bool {
        let mut guard = self.selection.lock();

        let had_selection = guard.is_some();

        *guard = Some(Selection::new(selection_type, pos, side));

        had_selection || selection_type != SelectionType::Simple
    }

    fn update_selection(&self, pos: Pos, side: Side) -> bool {
        let mut guard = self.selection.lock();

        let Some(selection) = guard.as_mut() else {
            return false;
        };

        selection.update(pos, side);

        true
    }

    fn finish_selection(&self) -> bool {
        let mut guard = self.selection.lock();

        if guard.as_ref().is_some_and(Selection::is_empty) {
            *guard = None;
        }

        false
    }

    /// Selected text via the engine formatter. Ranges reaching into scrollback
    /// extract real content instead of stopping at the viewport.
    fn selection_text(&self) -> Option<String> {
        let range = self.selection_screen_range()?;

        if range.start.row.0 < 0 || range.end.row.0 < 0 {
            return None;
        }

        self.session
            .engine
            .lock()
            .format_screen_range(
                (range.start.col.0 as u16, range.start.row.0 as u32),
                (range.end.col.0 as u16, range.end.row.0 as u32),
                range.is_block,
                // Rejoin soft-wrapped lines and drop trailing blanks, matching
                // the prior hand-rolled trim behavior.
                true,
                true,
            )
            .ok()
    }

    pub(super) fn copy_selection(&self) -> bool {
        let Some(text) = self.selection_text() else {
            return false;
        };

        if text.is_empty() {
            return false;
        }

        let mut clipboard = Clipboard::default();

        clipboard.set(ClipboardType::Clipboard, text);

        self.clear_selection();

        true
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

pub(super) fn block_selection_range(
    block: &BlockRef,
    palette: &Palette,
    line: usize,
    col: u32,
    selection_type: SelectionType,
) -> Option<((usize, u32), (usize, u32))> {
    let cols = usize::from(block.cols());

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

    if selection_type == SelectionType::Lines {
        return Some(((first, 0), (last, cols.saturating_sub(1) as u32)));
    }

    if selection_type != SelectionType::Semantic {
        let col = col.min(cols.saturating_sub(1) as u32);
        return Some(((line, col), (line, col)));
    }

    // Class 0 = whitespace, 1 = punctuation delimiter, 2 = word content.
    // Expanding one class matches terminal double-click behavior for words,
    // delimiter runs, and blank runs while retaining cell-accurate wide text.
    let mut classes = vec![0u8; (last - first + 1) * cols];

    for row in first..=last {
        let offset = (row - first) * cols;

        block
            .read_row_visit(row, palette, |x, text, wide, _| {
                use nmt_terminal::ghostty::CellWide;

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

                let x = usize::from(x);

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
