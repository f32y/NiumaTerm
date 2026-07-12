// grid/mod.rs was originally taken from Alacritty
// https://github.com/alacritty/alacritty/blob/e35e5ad14fce8456afdd89f2b392b9924bb27471/alacritty_terminal/src/grid/mod.rs
// which is licensed under Apache 2.0 license.
//
// The `Grid<T>`/`Storage`/resize engine and iterators were deleted with the
// `Crosswords` VT engine (the Ghostty engine + RenderBuffer own grid state now).
// What remains is the cell-geometry surface the live code still uses: the
// `Dimensions` trait, the `GridSquare` trait (`Row<T>`'s bound), `Row` (in
// `row`), and `Indexed` (selection).

pub mod row;

use std::ops::Deref;

use crate::terminal::pos::Pos;
use crate::terminal::{Column, Line};

pub trait GridSquare: Sized {
    fn is_empty(&self) -> bool;
    fn reset(&mut self, template: &Self);
}

pub trait Dimensions {
    /// Total number of lines in the buffer, this includes scrollback and visible lines.
    fn total_lines(&self) -> usize;

    /// Height of the viewport in lines.
    fn screen_lines(&self) -> usize;

    /// Width of the terminal in columns.
    fn columns(&self) -> usize;

    /// Index for the last column.
    #[inline]
    fn last_column(&self) -> Column {
        Column(self.columns() - 1)
    }

    /// Line farthest up in the grid history.
    #[inline]
    fn topmost_line(&self) -> Line {
        Line(-(self.history_size() as i32))
    }

    /// Line farthest down in the grid history.
    #[inline]
    fn bottommost_line(&self) -> Line {
        Line(self.screen_lines() as i32 - 1)
    }

    /// Number of invisible lines part of the scrollback history.
    #[inline]
    fn history_size(&self) -> usize {
        self.total_lines().saturating_sub(self.screen_lines())
    }

    /// square height in pixels.
    #[inline]
    fn square_height(&self) -> f32 {
        0.0
    }

    /// square width in pixels.
    #[inline]
    fn square_width(&self) -> f32 {
        0.0
    }
}

#[cfg(test)]
impl Dimensions for (usize, usize) {
    fn total_lines(&self) -> usize {
        self.0
    }
    fn screen_lines(&self) -> usize {
        self.0
    }
    fn columns(&self) -> usize {
        self.1
    }
    fn square_width(&self) -> f32 {
        0.
    }
    fn square_height(&self) -> f32 {
        0.
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Indexed<T> {
    pub pos: Pos,
    pub square: T,
}

impl<T> Deref for Indexed<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.square
    }
}
