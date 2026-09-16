//! Grid primitives shared by the render buffer and the selection code: the
//! packed cell and its side tables, the interned style table, rows of cells,
//! and grid coordinates.

#[cfg(test)]
#[path = "grid_tests.rs"]
mod grid_tests;

use std::cmp::{Ord, Ordering};
use std::ops::{
    Add, AddAssign, Deref, Index, IndexMut, Range, RangeFrom, RangeFull, RangeTo, RangeToInclusive,
    Sub, SubAssign,
};
use std::sync::Arc;
use std::sync::atomic::{self, AtomicU32};
use std::{fmt, slice};

use bitflags::bitflags;
use nmt_config::colors::{AnsiColor, NamedColor};
use rustc_hash::FxHashMap;
use tracing::warn;

use crate::ghostty::{SnapshotStyle, Underline};

// ---------------------------------------------------------------------------
// Coordinates
//
// Direction, Side and Column
// were taken originally from Alacritty https://github.com/alacritty/alacritty/blob/e35e5ad14fce8456afdd89f2b392b9924bb27471/alacritty_terminal/src/index.rs#L19
// which is licensed under Apache 2.0 license.
// ---------------------------------------------------------------------------

pub type Side = Direction;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Direction {
    Left,
    Right,
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

// ---------------------------------------------------------------------------
// Cells
//
// Copyright (c) 2023-present, Raphael Amorim.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.
//
// Rewritten from the upstream rio design as a packed `u64`. The old design held
// `c`, `fg`, `bg`, `flags`, and an `Option<Arc<CellExtra>>` inline (24 bytes
// per cell + per-cell heap allocations for "extras"). All variable-sized
// data now lives in per-grid side tables (`StyleSet`, `ExtrasTable`); the
// cell itself is 8 bytes.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Bit layout for Square(u64)
//
// bits 0..20 (21): codepoint (Unicode scalar value, max 0x10_FFFF)
// bits 21..22 (2): wide (Wide enum)
// bits 23..29 (7): per-cell flag bits (CellFlags), incl WRAPLINE at bit 0
// bits 30..31 (2): reserved
// bits 32..47 (16): style_id
// bits 48..63 (16): extras_id
//
// Background-only cells (a colored blank after `clear`, padding, color
// blocks) carry their background through the style table like any other
// cell; the engine read resolves the engine's bg-only cell tags into a
// style before the cell reaches this layout.
// ---------------------------------------------------------------------------

const CODEPOINT_SHIFT: u64 = 0;
const CODEPOINT_MASK: u64 = (1 << 21) - 1;

const WIDE_SHIFT: u64 = 21;
const WIDE_MASK: u64 = 0b11 << WIDE_SHIFT;

const CELL_FLAGS_SHIFT: u64 = 23;
const CELL_FLAGS_MASK: u64 = 0x7F << CELL_FLAGS_SHIFT; // 7 bits incl WRAPLINE

const STYLE_ID_SHIFT: u64 = 32;
const STYLE_ID_MASK: u64 = 0xFFFF << STYLE_ID_SHIFT;

const EXTRAS_ID_SHIFT: u64 = 48;
const EXTRAS_ID_MASK: u64 = 0xFFFF << EXTRAS_ID_SHIFT;

/// Wide-character state for a cell. Encoded in 2 bits.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Wide {
    /// Normal single-cell character.
    Narrow = 0,
    /// First cell of a double-wide character.
    Wide = 1,
    /// Second cell of a double-wide character.
    Spacer = 2,
    /// Trailing spacer at end of a soft-wrapped line indicating a wide
    /// character continues on the next line.
    LeadingSpacer = 3,
}

bitflags! {
 /// Per-cell flags that DON'T live in the style table. SGR-related
 /// attributes (bold, italic, underline, etc.) live in `StyleFlags`.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct CellFlags: u8 {
 /// Soft-wrap continuation marker on the last cell of a wrapped line.
        const WRAPLINE         = 1 << 0;

 /// Cell carries hyperlink metadata. Lookup via extras_id.
        const HYPERLINK        = 1 << 2;

 /// Cell carries multi-codepoint grapheme cluster. Lookup via extras_id.
        const GRAPHEME         = 1 << 3;
    }
}

/// Counter for hyperlinks without explicit ID.
static HYPERLINK_ID_SUFFIX: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hyperlink {
    inner: Arc<HyperlinkInner>,
}

impl Hyperlink {
    pub fn new<T: ToString>(id: Option<T>, uri: T) -> Self {
        let inner = Arc::new(HyperlinkInner::new(id, uri));

        Self { inner }
    }

    pub fn id(&self) -> &str {
        &self.inner.id
    }

    pub fn uri(&self) -> &str {
        &self.inner.uri
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct HyperlinkInner {
    id: String,
    uri: String,
}

impl HyperlinkInner {
    pub fn new<T: ToString>(id: Option<T>, uri: T) -> Self {
        let id = match id {
            Some(id) => id.to_string(),
            None => {
                let mut id = HYPERLINK_ID_SUFFIX
                    .fetch_add(1, atomic::Ordering::Relaxed)
                    .to_string();

                id.push_str("_yt");

                id
            }
        };

        Self {
            id,
            uri: uri.to_string(),
        }
    }
}

/// Index into `Grid::extras_table`. `0` means "no extras".
pub type ExtrasId = u16;

/// Storage for the rare per-cell data that used to live inside `CellExtra`.
/// Allocated only for cells that need it; pooled in a `Vec` on the grid.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct Extras {
    pub zerowidth: Vec<char>,
    pub hyperlink: Option<Hyperlink>,
}

impl Extras {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.zerowidth.is_empty() && self.hyperlink.is_none()
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Square(u64);

impl Default for Square {
    #[inline]
    fn default() -> Square {
        Square(0)
    }
}

impl Square {
    /// Read the underlying packed bits. Used by render hot loops that want
    /// to extract multiple fields from a single cell load — calling the
    /// individual accessors would otherwise reload the cell from memory
    /// each time the optimizer can't prove they alias.
    #[inline(always)]
    pub fn raw(self) -> u64 {
        self.0
    }

    #[inline]
    pub fn c(self) -> char {
        let cp = ((self.0 >> CODEPOINT_SHIFT) & CODEPOINT_MASK) as u32;

        // Safety: we only ever store valid Unicode scalar values via set_c.
        char::from_u32(cp).unwrap_or('\0')
    }

    #[inline]
    pub fn set_c(&mut self, c: char) {
        let cp = c as u32 as u64;

        debug_assert!(cp <= CODEPOINT_MASK, "codepoint exceeds 21 bits");

        self.0 = (self.0 & !CODEPOINT_MASK) | ((cp & CODEPOINT_MASK) << CODEPOINT_SHIFT);
    }

    #[inline]
    pub fn wide(self) -> Wide {
        self.0.into()
    }

    #[inline]
    pub fn set_wide(&mut self, w: Wide) {
        self.0 = (self.0 & !WIDE_MASK) | ((w as u64) << WIDE_SHIFT);
    }

    #[inline]
    pub fn cell_flags(self) -> CellFlags {
        let bits = ((self.0 & CELL_FLAGS_MASK) >> CELL_FLAGS_SHIFT) as u8;

        CellFlags::from_bits_truncate(bits)
    }

    #[inline]
    pub fn set_cell_flags(&mut self, f: CellFlags) {
        self.0 = (self.0 & !CELL_FLAGS_MASK) | ((f.bits() as u64) << CELL_FLAGS_SHIFT);
    }

    #[inline]
    pub fn contains_cell_flag(self, f: CellFlags) -> bool {
        self.cell_flags().contains(f)
    }

    /// Read the style id bits.
    #[inline(always)]
    pub fn style_id(self) -> StyleId {
        ((self.0 & STYLE_ID_MASK) >> STYLE_ID_SHIFT) as StyleId
    }

    #[inline]
    pub fn set_style_id(&mut self, id: StyleId) {
        self.0 = (self.0 & !STYLE_ID_MASK) | ((id as u64) << STYLE_ID_SHIFT);
    }

    /// Read the cell's extras id, if any.
    #[inline(always)]
    pub fn extras_id(self) -> Option<ExtrasId> {
        let id = ((self.0 & EXTRAS_ID_MASK) >> EXTRAS_ID_SHIFT) as ExtrasId;

        if id == 0 { None } else { Some(id) }
    }

    #[inline]
    pub fn set_extras_id(&mut self, id: Option<ExtrasId>) {
        let bits = id.unwrap_or(0) as u64;

        self.0 = (self.0 & !EXTRAS_ID_MASK) | (bits << EXTRAS_ID_SHIFT);
    }

    /// Clear all per-cell state. Used by `clear_wide` and similar.
    #[inline]
    pub fn clear(&mut self) {
        *self = Square(0);
    }

    /// Builder helper for tests.
    #[inline]
    pub fn with_style_id(mut self, id: StyleId) -> Self {
        self.set_style_id(id);

        self
    }

    #[inline]
    pub fn is_wide(self) -> bool {
        matches!(self.wide(), Wide::Wide)
    }

    #[inline]
    pub fn is_spacer(self) -> bool {
        matches!(self.wide(), Wide::Spacer)
    }

    #[inline]
    pub fn wrapline(self) -> bool {
        self.contains_cell_flag(CellFlags::WRAPLINE)
    }

    #[inline]
    pub fn set_wrapline(&mut self, on: bool) {
        let mut flags = self.cell_flags();

        flags.set(CellFlags::WRAPLINE, on);

        self.set_cell_flags(flags);
    }

    #[inline]
    pub fn has_extras(self) -> bool {
        self.extras_id().is_some()
    }

    #[inline]
    pub fn has_grapheme(self) -> bool {
        self.contains_cell_flag(CellFlags::GRAPHEME)
    }

    #[inline]
    pub fn has_hyperlink(self) -> bool {
        self.contains_cell_flag(CellFlags::HYPERLINK)
    }
}

impl From<char> for Square {
    /// Create a cell with the given codepoint and the default style/extras.
    #[inline]
    fn from(c: char) -> Self {
        let mut s = Square(0);

        s.set_c(c);

        s
    }
}

impl From<u64> for Wide {
    #[inline]
    fn from(bits: u64) -> Self {
        match (bits >> WIDE_SHIFT) & 0b11 {
            0 => Wide::Narrow,
            1 => Wide::Wide,
            2 => Wide::Spacer,
            _ => Wide::LeadingSpacer,
        }
    }
}

// ---------------------------------------------------------------------------
// Styles
//
// Per-grid style intern table for cells.
//
// Each unique combination of (fg, bg, underline_color, sgr_flags) is hashed
// and assigned a `StyleId: u16`. The cell stores only the id; the actual
// style data lives in `StyleSet::styles`. This means:
//
//   - Most cells share `style_id == 0` (the default style) and require no
//     lookup at render time.
//   - A row of 200 characters with the same SGR state shares one id.
//   - The `Square` struct stays at u64.
// ---------------------------------------------------------------------------

/// Index into the per-grid `StyleSet`. Id `0` is always the default style
/// (`Style::default()`), so a freshly-zeroed cell renders correctly without
/// any lookup.
pub type StyleId = u16;

/// The id of the default style. Always present.
pub const DEFAULT_STYLE_ID: StyleId = 0;

bitflags! {
    /// SGR-related cell attributes that live inside the style table.
    /// Distinct from the per-cell flags that stay on `Square` (wide,
    /// wrapline, presence bits).
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
    pub struct StyleFlags: u16 {
        const INVERSE          = 1 << 0;
        const BOLD             = 1 << 1;
        const ITALIC           = 1 << 2;
        const DIM              = 1 << 3;
        const HIDDEN           = 1 << 4;
        const STRIKEOUT        = 1 << 5;

        // 3-bit underline kind packed into bits 6-8.
        const UNDERLINE        = 1 << 6;
        const DOUBLE_UNDERLINE = 1 << 7;
        const UNDERCURL        = 1 << 8;
        const DOTTED_UNDERLINE = 1 << 9;
        const DASHED_UNDERLINE = 1 << 10;

        const ALL_UNDERLINES   = Self::UNDERLINE.bits()
                               | Self::DOUBLE_UNDERLINE.bits()
                               | Self::UNDERCURL.bits()
                               | Self::DOTTED_UNDERLINE.bits()
                               | Self::DASHED_UNDERLINE.bits();

        // Combined intensity for shaping decisions.
        const DIM_BOLD         = Self::DIM.bits() | Self::BOLD.bits();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Style {
    pub fg: AnsiColor,
    pub bg: AnsiColor,
    pub underline_color: Option<AnsiColor>,
    pub flags: StyleFlags,
}

impl Default for Style {
    #[inline]
    fn default() -> Self {
        Self {
            fg: AnsiColor::Named(NamedColor::Foreground),
            bg: AnsiColor::Named(NamedColor::Background),
            underline_color: None,
            flags: StyleFlags::empty(),
        }
    }
}

/// Interning table for `Style` values, owned per-grid.
#[derive(Clone, Debug)]
pub struct StyleSet {
    styles: Vec<Style>,
    lookup: FxHashMap<Style, StyleId>,
}

impl PartialEq for StyleSet {
    fn eq(&self, other: &Self) -> bool {
        // Two style sets are considered equal if they intern the same set
        // of styles in the same id order. Used by snapshot diffing.
        self.styles == other.styles
    }
}

impl StyleSet {
    /// Create a new style set with the default style pre-interned at id 0.
    pub fn new() -> Self {
        let default_style = Style::default();

        let mut lookup = FxHashMap::default();

        lookup.insert(default_style, DEFAULT_STYLE_ID);

        Self {
            styles: vec![default_style],
            lookup,
        }
    }

    /// Look up the style for an id. Returns the default style for unknown
    /// ids (defensive — should never happen in practice).
    ///
    /// Hot path note: this is called once per cell during rendering, so the
    /// `id == 0` (default style) check is intentionally inlined first. The
    /// overwhelming majority of cells in a typical terminal use the default
    /// style; that branch becomes a single compare + copy of a constant.
    #[inline(always)]
    pub fn get(&self, id: StyleId) -> Style {
        if id == DEFAULT_STYLE_ID {
            return Style::default();
        }

        // Safety: ids are only ever produced by `intern`, which guarantees
        // they index into `self.styles`. The bounds check is provably dead
        // on the hot path but the optimizer doesn't always remove it.
        // We still fall back to the default if the slot is somehow gone.
        self.styles
            .get(id as usize)
            .copied()
            .unwrap_or_else(Style::default)
    }

    /// Unchecked variant of `get`. Skips both the default-style early
    /// return AND the bounds check on `self.styles`. Used by the renderer
    /// hot loop after the caller has already verified the id is non-zero
    /// and in range (which is always true for ids produced by `intern`).
    ///
    /// # Safety
    /// `id` must be a valid index into `self.styles` (i.e. less than
    /// `self.len()`). Ids returned by `intern` always satisfy this.
    #[inline(always)]
    pub unsafe fn get_unchecked(&self, id: StyleId) -> Style {
        debug_assert!(
            (id as usize) < self.styles.len(),
            "StyleSet::get_unchecked called with out-of-range id {} (len {})",
            id,
            self.styles.len(),
        );

        unsafe { *self.styles.get_unchecked(id as usize) }
    }

    /// Intern a style and return its id. If the style already exists,
    /// returns the existing id. If not, inserts it.
    ///
    /// Saturates at `u16::MAX` styles per grid: any attempt to intern beyond
    /// that returns `DEFAULT_STYLE_ID`. In practice sessions use < 100
    /// distinct styles so this is purely defensive.
    pub fn intern(&mut self, style: Style) -> StyleId {
        if let Some(&id) = self.lookup.get(&style) {
            return id;
        }

        if self.styles.len() >= u16::MAX as usize {
            warn!(
                "StyleSet hit u16::MAX styles ({}); falling back to default",
                self.styles.len()
            );

            return DEFAULT_STYLE_ID;
        }

        let id = self.styles.len() as StyleId;

        self.styles.push(style);

        self.lookup.insert(style, id);

        id
    }

    /// Reset to the initial state (default style at id 0), keeping the
    /// allocations. Interned ids become dangling; only call this when no
    /// stored ids survive (e.g. right after the grid was cleared).
    pub fn clear(&mut self) {
        self.styles.truncate(1);

        self.lookup.clear();

        self.lookup.insert(self.styles[0], DEFAULT_STYLE_ID);
    }

    #[inline]
    pub fn styles(&self) -> &[Style] {
        &self.styles
    }

    /// Number of distinct styles currently interned.
    #[inline]
    pub fn len(&self) -> usize {
        self.styles.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }
}

impl Default for StyleSet {
    fn default() -> Self {
        Self::new()
    }
}

impl From<&SnapshotStyle> for Style {
    /// Build a `Style` from a Ghostty snapshot style. Ghostty `blink` and
    /// `overline` have no render flag and are intentionally dropped rather than
    /// synthesizing unsupported styling.
    fn from(s: &SnapshotStyle) -> Self {
        let mut flags = StyleFlags::empty();

        flags.set(StyleFlags::BOLD, s.bold);

        flags.set(StyleFlags::ITALIC, s.italic);

        flags.set(StyleFlags::DIM, s.faint);

        flags.set(StyleFlags::INVERSE, s.inverse);

        flags.set(StyleFlags::HIDDEN, s.invisible);

        flags.set(StyleFlags::STRIKEOUT, s.strikethrough);

        flags |= match s.underline {
            Underline::None => StyleFlags::empty(),
            Underline::Single => StyleFlags::UNDERLINE,
            Underline::Double => StyleFlags::DOUBLE_UNDERLINE,
            Underline::Curly => StyleFlags::UNDERCURL,
            Underline::Dotted => StyleFlags::DOTTED_UNDERLINE,
            Underline::Dashed => StyleFlags::DASHED_UNDERLINE,
        };

        Style {
            fg: s
                .fg
                .map(AnsiColor::Spec)
                .unwrap_or(AnsiColor::Named(NamedColor::Foreground)),
            bg: s
                .bg
                .map(AnsiColor::Spec)
                .unwrap_or(AnsiColor::Named(NamedColor::Background)),
            underline_color: s.underline_color.map(AnsiColor::Spec),
            flags,
        }
    }
}

// ---------------------------------------------------------------------------
// Rows
//
// grid/row.rs was originally taken from Alacritty
// https://github.com/alacritty/alacritty/blob/e35e5ad14fce8456afdd89f2b392b9924bb27471/alacritty_terminal/src/grid/row.rs
// which is licensed under Apache 2.0 license.
// ---------------------------------------------------------------------------

/// A row in the grid.
#[derive(Clone, Debug)]
pub struct Row<T> {
    pub inner: Vec<T>,

    /// Set when at least one cell in the row contains a kitty Unicode
    /// graphics-protocol placeholder (U+10EEEE). The renderer skips the
    /// placeholder scan on rows where this is `false`.
    pub kitty_virtual_placeholder: bool,
}

impl<T> Default for Row<T> {
    fn default() -> Self {
        Self {
            inner: Vec::new(),
            kitty_virtual_placeholder: false,
        }
    }
}

impl<T: PartialEq> PartialEq for Row<T> {
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
}

impl<T> Row<T> {
    /// Create a new terminal row.
    pub fn new(columns: usize) -> Row<T>
    where
        T: Clone + Default,
    {
        // The previous hand-rolled pointer initialization was UB for
        // `columns == 0` (it wrote one element past a zero-capacity Vec) and
        // compiles to the same fill loop as the safe version below.
        let mut inner: Vec<T> = Vec::with_capacity(columns);

        inner.resize_with(columns, T::default);

        Row {
            inner,
            kitty_virtual_placeholder: false,
        }
    }

    #[allow(clippy::len_without_is_empty)]
    #[inline]
    pub fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<'a, T> IntoIterator for &'a Row<T> {
    type IntoIter = slice::Iter<'a, T>;

    type Item = &'a T;

    #[inline]
    fn into_iter(self) -> slice::Iter<'a, T> {
        self.inner.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut Row<T> {
    type IntoIter = slice::IterMut<'a, T>;

    type Item = &'a mut T;

    #[inline]
    fn into_iter(self) -> slice::IterMut<'a, T> {
        self.inner.iter_mut()
    }
}

impl<T> Index<Column> for Row<T> {
    type Output = T;

    #[inline]
    fn index(&self, index: Column) -> &T {
        &self.inner[index.0]
    }
}

impl<T> IndexMut<Column> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: Column) -> &mut T {
        &mut self.inner[index.0]
    }
}

impl<T> Index<Range<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: Range<Column>) -> &[T] {
        &self.inner[(index.start.0)..(index.end.0)]
    }
}

impl<T> IndexMut<Range<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: Range<Column>) -> &mut [T] {
        &mut self.inner[(index.start.0)..(index.end.0)]
    }
}

impl<T> Index<RangeTo<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeTo<Column>) -> &[T] {
        &self.inner[..(index.end.0)]
    }
}

impl<T> IndexMut<RangeTo<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeTo<Column>) -> &mut [T] {
        &mut self.inner[..(index.end.0)]
    }
}

impl<T> Index<RangeFrom<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeFrom<Column>) -> &[T] {
        &self.inner[(index.start.0)..]
    }
}

impl<T> IndexMut<RangeFrom<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeFrom<Column>) -> &mut [T] {
        &mut self.inner[(index.start.0)..]
    }
}

impl<T> Index<RangeFull> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, _: RangeFull) -> &[T] {
        &self.inner[..]
    }
}

impl<T> IndexMut<RangeFull> for Row<T> {
    #[inline]
    fn index_mut(&mut self, _: RangeFull) -> &mut [T] {
        &mut self.inner[..]
    }
}

impl<T> Index<RangeToInclusive<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeToInclusive<Column>) -> &[T] {
        &self.inner[..=(index.end.0)]
    }
}

impl<T> IndexMut<RangeToInclusive<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeToInclusive<Column>) -> &mut [T] {
        &mut self.inner[..=(index.end.0)]
    }
}
