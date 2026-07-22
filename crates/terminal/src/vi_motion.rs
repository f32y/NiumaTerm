//! Vi-mode cursor motions over the render buffer's visible grid.
//! Ports `crosswords::vi_mode` to **viewport-local free functions** over
//! [`VisibleGrid`]. The vi cursor is a SCREEN coordinate at the call site; these
//! functions operate in visible-row coordinates and **clamp at the viewport
//! edge** — the frontend handler drives `scroll_viewport` + a synchronous render-
//! buffer refill to cross into scrollback, keeping motion reads bounded. Reuses
//! `VisibleGrid`'s semantic/line/bracket searches; adds the vi-only helpers
//! (`expand_wide`, occupied-cell scans, whitespace/word stepping).

use std::cmp::min;

use crate::selection_search::VisibleGrid;
use crate::terminal::pos::{Column, Direction, Line, Pos, Side};
use crate::terminal::square::Wide;

/// Vi mode motion movements (mirrors the legacy `crosswords::vi_mode::ViMotion`).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ViMotion {
    /// Move up.
    Up,
    /// Move down.
    Down,
    /// Move left.
    Left,
    /// Move right.
    Right,
    /// Move to start of line.
    First,
    /// Move to end of line.
    Last,
    /// Move to the first non-empty cell.
    FirstOccupied,
    /// Move to top of screen.
    High,
    /// Move to center of screen.
    Middle,
    /// Move to bottom of screen.
    Low,
    /// Move to start of semantically separated word.
    SemanticLeft,
    /// Move to start of next semantically separated word.
    SemanticRight,
    /// Move to end of previous semantically separated word.
    SemanticLeftEnd,
    /// Move to end of semantically separated word.
    SemanticRightEnd,
    /// Move to start of whitespace separated word.
    WordLeft,
    /// Move to start of next whitespace separated word.
    WordRight,
    /// Move to end of previous whitespace separated word.
    WordLeftEnd,
    /// Move to end of whitespace separated word.
    WordRightEnd,
    /// Move to opposing bracket.
    Bracket,
}

/// Apply a vi `motion` to `pos` (visible-row coords) over the visible grid.
/// `escape_chars` is the semantic-word separator set. The result is clamped to
/// the viewport; the caller scrolls + re-fills to continue past an edge.
#[must_use]
pub fn vi_motion(g: &VisibleGrid, escape_chars: &str, mut pos: Pos, motion: ViMotion) -> Pos {
    let last_column = Column(g.last_col());
    match motion {
        ViMotion::Up => {
            if pos.row.0 > 0 {
                pos.row -= 1;
            }
        }
        ViMotion::Down => {
            if pos.row.0 + 1 < g.rows_len() {
                pos.row += 1;
            }
        }
        ViMotion::Left => {
            pos = expand_wide(g, pos, Direction::Left);
            let wrap_pos = Pos::new(pos.row - 1, last_column);
            if pos.col.0 == 0 && pos.row.0 > 0 && is_wrap(g, wrap_pos) {
                pos = wrap_pos;
            } else {
                pos.col = Column(pos.col.0.saturating_sub(1));
            }
        }
        ViMotion::Right => {
            pos = expand_wide(g, pos, Direction::Right);
            if is_wrap(g, pos) {
                pos = Pos::new(pos.row + 1, Column(0));
            } else {
                pos.col = min(pos.col + 1, last_column);
            }
        }
        ViMotion::First => {
            pos = expand_wide(g, pos, Direction::Left);
            while pos.col.0 == 0 && pos.row.0 > 0 && is_wrap(g, Pos::new(pos.row - 1, last_column))
            {
                pos.row -= 1;
            }
            pos.col = Column(0);
        }
        ViMotion::Last => pos = last(g, pos),
        ViMotion::FirstOccupied => pos = first_occupied(g, pos),
        ViMotion::High => {
            let line = 0;
            let col = first_occupied_in_line(g, line).unwrap_or_default().col;
            pos = Pos::new(Line(line), col);
        }
        ViMotion::Middle => {
            // saturating_sub: a one-row viewport would otherwise underflow.
            let line = (g.rows_len() / 2).saturating_sub(1);
            let col = first_occupied_in_line(g, line).unwrap_or_default().col;
            pos = Pos::new(Line(line), col);
        }
        ViMotion::Low => {
            let line = g.rows_len() - 1;
            let col = first_occupied_in_line(g, line).unwrap_or_default().col;
            pos = Pos::new(Line(line), col);
        }
        ViMotion::SemanticLeft => {
            pos = semantic(g, escape_chars, pos, Direction::Left, Side::Left);
        }
        ViMotion::SemanticRight => {
            pos = semantic(g, escape_chars, pos, Direction::Right, Side::Left);
        }
        ViMotion::SemanticLeftEnd => {
            pos = semantic(g, escape_chars, pos, Direction::Left, Side::Right);
        }
        ViMotion::SemanticRightEnd => {
            pos = semantic(g, escape_chars, pos, Direction::Right, Side::Right);
        }
        ViMotion::WordLeft => {
            pos = word(g, pos, Direction::Left, Side::Left);
        }
        ViMotion::WordRight => {
            pos = word(g, pos, Direction::Right, Side::Left);
        }
        ViMotion::WordLeftEnd => {
            pos = word(g, pos, Direction::Left, Side::Right);
        }
        ViMotion::WordRightEnd => {
            pos = word(g, pos, Direction::Right, Side::Right);
        }
        ViMotion::Bracket => pos = g.bracket_search(pos).unwrap_or(pos),
    }
    pos
}

/// Jump to the end of a wide cell (viewport-local port of
/// `Crosswords::expand_wide`).
fn expand_wide(g: &VisibleGrid, mut pos: Pos, direction: Direction) -> Pos {
    let last_column = Column(g.last_col());
    let wide = g.cell(pos).wide();
    match direction {
        Direction::Right if matches!(wide, Wide::LeadingSpacer) => {
            pos.col = Column(1);
            pos.row += 1;
        }
        Direction::Right if matches!(wide, Wide::Wide) => {
            pos.col = min(pos.col + 1, last_column);
        }
        Direction::Left if matches!(wide, Wide::Wide | Wide::Spacer) => {
            if matches!(wide, Wide::Spacer) {
                pos.col = Column(pos.col.0.saturating_sub(1));
            }
            if let Some(prev) = g.prev(pos) {
                if matches!(g.cell(prev).wide(), Wide::LeadingSpacer) {
                    pos = prev;
                }
            }
        }
        _ => (),
    }
    pos
}

/// Find next end of line to move to.
fn last(g: &VisibleGrid, mut pos: Pos) -> Pos {
    pos = expand_wide(g, pos, Direction::Right);
    let occupied = last_occupied_in_line(g, pos.row.0).unwrap_or_default();
    if pos.col < occupied.col {
        occupied
    } else if is_wrap(g, pos) {
        while is_wrap(g, pos) {
            pos.row += 1;
        }
        last_occupied_in_line(g, pos.row.0).unwrap_or(pos)
    } else {
        Pos::new(pos.row, Column(g.last_col()))
    }
}

/// Find next non-empty cell to move to.
fn first_occupied(g: &VisibleGrid, mut pos: Pos) -> Pos {
    let last_column = Column(g.last_col());
    pos = expand_wide(g, pos, Direction::Left);
    let occupied =
        first_occupied_in_line(g, pos.row.0).unwrap_or_else(|| Pos::new(pos.row, last_column));

    if pos == occupied {
        let mut occupied = None;
        for line in (0..pos.row.0).rev() {
            if !is_wrap(g, Pos::new(Line(line), last_column)) {
                break;
            }
            occupied = first_occupied_in_line(g, line).or(occupied);
        }

        let mut line = pos.row.0;
        occupied.unwrap_or_else(|| {
            loop {
                if let Some(occupied) = first_occupied_in_line(g, line) {
                    break occupied;
                }
                let last_cell = Pos::new(Line(line), last_column);
                if !is_wrap(g, last_cell) {
                    break last_cell;
                }
                line += 1;
            }
        })
    } else {
        occupied
    }
}

/// Move by semantically separated word, like w/b/e/ge in vi.
fn semantic(
    g: &VisibleGrid,
    escape_chars: &str,
    mut pos: Pos,
    direction: Direction,
    side: Side,
) -> Pos {
    let expand_semantic = |pos: Pos| {
        let cell = g.cell(pos);
        if escape_chars.contains(cell.c())
            && !matches!(cell.wide(), Wide::Spacer | Wide::LeadingSpacer)
        {
            pos
        } else if direction == Direction::Left {
            g.semantic_search_left(pos, escape_chars)
        } else {
            g.semantic_search_right(pos, escape_chars)
        }
    };

    pos = expand_wide(g, pos, direction);

    if direction != side && !is_boundary(g, pos, direction) {
        pos = expand_semantic(pos);
    }

    let mut next_pos = advance(g, pos, direction);
    while !is_boundary(g, pos, direction) && is_space(g, next_pos) {
        pos = next_pos;
        next_pos = advance(g, pos, direction);
    }

    if !is_boundary(g, pos, direction) {
        pos = advance(g, pos, direction);
    }

    if direction == side && !is_boundary(g, pos, direction) {
        pos = expand_semantic(pos);
    }

    pos
}

/// Move by whitespace separated word, like W/B/E/gE in vi.
fn word(g: &VisibleGrid, mut pos: Pos, direction: Direction, side: Side) -> Pos {
    pos = expand_wide(g, pos, direction);

    if direction == side {
        let mut next_pos = advance(g, pos, direction);
        while !is_boundary(g, pos, direction) && is_space(g, next_pos) {
            pos = next_pos;
            next_pos = advance(g, pos, direction);
        }

        let mut next_pos = advance(g, pos, direction);
        while !is_boundary(g, pos, direction) && !is_space(g, next_pos) {
            pos = next_pos;
            next_pos = advance(g, pos, direction);
        }
    }

    if direction != side {
        while !is_boundary(g, pos, direction) && !is_space(g, pos) {
            pos = advance(g, pos, direction);
        }
        while !is_boundary(g, pos, direction) && is_space(g, pos) {
            pos = advance(g, pos, direction);
        }
    }

    pos
}

/// Find first non-empty cell in line `row`.
fn first_occupied_in_line(g: &VisibleGrid, row: i32) -> Option<Pos> {
    (0..g.cols())
        .map(|col| Pos::new(Line(row), Column(col)))
        .find(|&pos| !is_space(g, pos))
}

/// Find last non-empty cell in line `row`.
fn last_occupied_in_line(g: &VisibleGrid, row: i32) -> Option<Pos> {
    (0..g.cols())
        .map(|col| Pos::new(Line(row), Column(col)))
        .rfind(|&pos| !is_space(g, pos))
}

/// Advance pos by one cell in `direction`, clamping at the grid edge.
fn advance(g: &VisibleGrid, pos: Pos, direction: Direction) -> Pos {
    if direction == Direction::Left {
        g.prev(pos).unwrap_or(pos)
    } else {
        g.next(pos).unwrap_or(pos)
    }
}

/// Whether the cell at `pos` is whitespace (not a wide spacer).
fn is_space(g: &VisibleGrid, pos: Pos) -> bool {
    let cell = g.cell(pos);
    !matches!(cell.wide(), Wide::Spacer | Wide::LeadingSpacer)
        && (cell.c() == '\0' || cell.c() == ' ' || cell.c() == '\t')
}

/// Whether the row at `pos` soft-wraps into the next row, at the wrap cell
/// (last column) — matching the legacy per-cell `WRAPLINE` semantics over the
/// render buffer's per-row wrap flag.
fn is_wrap(g: &VisibleGrid, pos: Pos) -> bool {
    pos.col.0 == g.last_col() && g.wrapped(pos.row.0)
}

/// Whether `pos` is at the viewport boundary in `direction`.
fn is_boundary(g: &VisibleGrid, pos: Pos, direction: Direction) -> bool {
    (pos.row.0 <= 0 && pos.col.0 == 0 && direction == Direction::Left)
        || (pos.row.0 == g.rows_len() - 1
            && pos.col.0 + 1 >= g.cols()
            && direction == Direction::Right)
}
