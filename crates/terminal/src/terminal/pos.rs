// Direction, Side, CursorState and Column
// were taken originally from Alacritty https://github.com/alacritty/alacritty/blob/e35e5ad14fce8456afdd89f2b392b9924bb27471/alacritty_terminal/src/index.rs#L19
// which is licensed under Apache 2.0 license.

use std::cmp::{Ord, Ordering};
use std::fmt;
use std::ops::{Add, AddAssign, Deref, Sub, SubAssign};

use crate::ansi::CursorShape;

pub type Side = Direction;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Direction {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CursorState {
    pub pos: Pos,
    pub content: CursorShape,
}

impl CursorState {
    pub fn is_visible(&self) -> bool {
        self.content != CursorShape::Hidden
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialOrd, PartialEq)]
pub struct Pos<L = Line, C = Column> {
    pub row: L,
    pub col: C,
}

impl<L, C> Pos<L, C> {
    pub fn new(row: L, col: C) -> Pos<L, C> {
        Pos { row, col }
    }
}

/// A line.
///
/// Newtype to avoid passing values incorrectly.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default, Ord, PartialOrd)]
pub struct Line(pub i32);

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<usize> for Line {
    fn from(source: usize) -> Self {
        Self(source as i32)
    }
}

impl Add<usize> for Line {
    type Output = Line;

    #[inline]
    fn add(self, rhs: usize) -> Line {
        self + rhs as i32
    }
}

impl AddAssign<usize> for Line {
    #[inline]
    fn add_assign(&mut self, rhs: usize) {
        *self += rhs as i32;
    }
}

impl Sub<usize> for Line {
    type Output = Line;

    #[inline]
    fn sub(self, rhs: usize) -> Line {
        self - rhs as i32
    }
}

impl SubAssign<usize> for Line {
    #[inline]
    fn sub_assign(&mut self, rhs: usize) {
        *self -= rhs as i32;
    }
}

impl PartialOrd<usize> for Line {
    #[inline]
    fn partial_cmp(&self, other: &usize) -> Option<Ordering> {
        self.0.partial_cmp(&(*other as i32))
    }
}

impl PartialEq<usize> for Line {
    #[inline]
    fn eq(&self, other: &usize) -> bool {
        self.0.eq(&(*other as i32))
    }
}

/// A column.
///
/// Newtype to avoid passing values incorrectly.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default, Ord, PartialOrd)]
pub struct Column(pub usize);

impl fmt::Display for Column {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

macro_rules! ops {
    ($ty:ty, $construct:expr, $primitive:ty) => {
        impl Deref for $ty {
            type Target = $primitive;

            #[inline]
            fn deref(&self) -> &$primitive {
                &self.0
            }
        }

        impl From<$primitive> for $ty {
            #[inline]
            fn from(val: $primitive) -> $ty {
                $construct(val)
            }
        }

        impl Add<$ty> for $ty {
            type Output = $ty;

            #[inline]
            fn add(self, rhs: $ty) -> $ty {
                $construct(self.0 + rhs.0)
            }
        }

        impl AddAssign<$ty> for $ty {
            #[inline]
            fn add_assign(&mut self, rhs: $ty) {
                self.0 += rhs.0;
            }
        }

        impl Add<$primitive> for $ty {
            type Output = $ty;

            #[inline]
            fn add(self, rhs: $primitive) -> $ty {
                $construct(self.0 + rhs)
            }
        }

        impl AddAssign<$primitive> for $ty {
            #[inline]
            fn add_assign(&mut self, rhs: $primitive) {
                self.0 += rhs
            }
        }

        impl Sub<$ty> for $ty {
            type Output = $ty;

            #[inline]
            fn sub(self, rhs: $ty) -> $ty {
                $construct(self.0 - rhs.0)
            }
        }

        impl SubAssign<$ty> for $ty {
            #[inline]
            fn sub_assign(&mut self, rhs: $ty) {
                self.0 -= rhs.0;
            }
        }

        impl Sub<$primitive> for $ty {
            type Output = $ty;

            #[inline]
            fn sub(self, rhs: $primitive) -> $ty {
                $construct(self.0 - rhs)
            }
        }

        impl SubAssign<$primitive> for $ty {
            #[inline]
            fn sub_assign(&mut self, rhs: $primitive) {
                self.0 -= rhs
            }
        }

        impl PartialEq<$ty> for $primitive {
            #[inline]
            fn eq(&self, other: &$ty) -> bool {
                self.eq(&other.0)
            }
        }

        impl PartialEq<$primitive> for $ty {
            #[inline]
            fn eq(&self, other: &$primitive) -> bool {
                self.0.eq(other)
            }
        }

        impl PartialOrd<$ty> for $primitive {
            #[inline]
            fn partial_cmp(&self, other: &$ty) -> Option<Ordering> {
                self.partial_cmp(&other.0)
            }
        }

        impl PartialOrd<$primitive> for $ty {
            #[inline]
            fn partial_cmp(&self, other: &$primitive) -> Option<Ordering> {
                self.0.partial_cmp(other)
            }
        }
    };
}

ops!(Column, Column, usize);

ops!(Line, Line, i32);

impl From<char> for CursorState {
    fn from(cursor: char) -> Self {
        CursorState {
            pos: Pos::default(),
            content: cursor.into(),
        }
    }
}
