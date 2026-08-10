use std::{ops, str};

use libghostty_vt_sys::{
    CellWide as VtCellWide, ColorRgb as VtColorRgb, SgrUnderline as VtSgrUnderline,
    ghostty_color_rgb_get,
};
use nmt_config::colors::ColorRgb;

use crate::ansi;

/// Alias of the workspace-wide RGB type; `VtColorRgb` is the FFI handle,
/// this is the decoded byte triple.
pub type Color = ColorRgb;

/// Decode an FFI color handle into its byte triple. A free function because
/// `Color` lives in `nmt_config` and `VtColorRgb` in the sys crate, so a
/// `From` impl would violate the orphan rule.
pub(super) fn color_from_vt(value: VtColorRgb) -> Color {
    let mut r = 0;
    let mut g = 0;
    let mut b = 0;

    unsafe {
        // ghostty 53bd14f: the accessor takes the color by const pointer.
        ghostty_color_rgb_get(&value, &mut r, &mut g, &mut b);
    }

    Color { r, g, b }
}

/// Style flags for a cell. `fg`/`bg` are `None` when the cell uses the
/// terminal default color (the caller resolves the default).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SnapshotStyle {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub underline: Underline,
    pub underline_color: Option<Color>,
}

/// Underline style, mirroring `VtSgrUnderline::*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Underline {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

impl From<VtSgrUnderline::Type> for Underline {
    fn from(value: VtSgrUnderline::Type) -> Self {
        match value {
            v if v == VtSgrUnderline::SINGLE => Self::Single,
            v if v == VtSgrUnderline::DOUBLE => Self::Double,
            v if v == VtSgrUnderline::CURLY => Self::Curly,
            v if v == VtSgrUnderline::DOTTED => Self::Dotted,
            v if v == VtSgrUnderline::DASHED => Self::Dashed,
            _ => Self::None,
        }
    }
}

/// Width classification of a cell, mirroring `VtCellWide::*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CellWide {
    #[default]
    Narrow,
    /// First cell of a double-width character.
    Wide,
    /// Second cell of a double-width character (no glyph).
    SpacerTail,
    /// Padding before a wide char at a soft-wrap boundary (no glyph).
    SpacerHead,
}

impl From<i32> for CellWide {
    fn from(value: i32) -> Self {
        match value {
            v if v == VtCellWide::WIDE => Self::Wide,
            v if v == VtCellWide::SPACER_TAIL => Self::SpacerTail,
            v if v == VtCellWide::SPACER_HEAD => Self::SpacerHead,
            _ => Self::Narrow,
        }
    }
}

/// The engine's resolved 256-color palette, fetched once per lock hold and
/// threaded through batch row reads (see `read_screen_row_visit`).
pub type Palette = [VtColorRgb; 256];

/// Cell text with inline storage for short content. Almost every cell is a
/// single codepoint (≤4 UTF-8 bytes); heap-allocating a `String` per cell was
/// the dominant harvest cost (61 ns/cell, 91% of PTY time under scroll floods).
/// Content up to 22 bytes — every single codepoint and all common grapheme
/// clusters — stays inline; longer clusters (rare ZWJ chains) spill to heap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellText(CellTextRepr);

#[derive(Debug, Clone, PartialEq, Eq)]
enum CellTextRepr {
    Inline { len: u8, buf: [u8; 22] },
    Heap(String),
}

impl CellText {
    pub fn from_char(c: char) -> Self {
        let mut buf = [0u8; 22];
        let len = c.encode_utf8(&mut buf).len() as u8;
        CellText(CellTextRepr::Inline { len, buf })
    }

    pub fn as_str(&self) -> &str {
        match &self.0 {
            // Invariant: constructors only store valid UTF-8 prefixes.
            CellTextRepr::Inline { len, buf } => unsafe {
                str::from_utf8_unchecked(&buf[..*len as usize])
            },
            CellTextRepr::Heap(s) => s,
        }
    }

    pub fn is_empty(&self) -> bool {
        match &self.0 {
            CellTextRepr::Inline { len, .. } => *len == 0,
            CellTextRepr::Heap(s) => s.is_empty(),
        }
    }
}

impl Default for CellText {
    fn default() -> Self {
        CellText(CellTextRepr::Inline {
            len: 0,
            buf: [0; 22],
        })
    }
}

impl From<&str> for CellText {
    fn from(s: &str) -> Self {
        if s.len() <= 22 {
            let mut buf = [0u8; 22];
            buf[..s.len()].copy_from_slice(s.as_bytes());
            CellText(CellTextRepr::Inline {
                len: s.len() as u8,
                buf,
            })
        } else {
            CellText(CellTextRepr::Heap(s.to_string()))
        }
    }
}

impl From<String> for CellText {
    fn from(s: String) -> Self {
        if s.len() <= 22 {
            CellText::from(s.as_str())
        } else {
            CellText(CellTextRepr::Heap(s))
        }
    }
}

impl ops::Deref for CellText {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

/// One sparse cell of a [`ScreenRowRead`], with inline [`CellText`] instead of
/// a per-cell `String`.
/// Test-only: production reads visit cells in place (`read_screen_row_visit`).
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowCell {
    pub x: u16,
    /// Grapheme cluster for the cell. Empty for blank cells.
    pub text: CellText,
    pub wide: CellWide,
    pub style: SnapshotStyle,
}

/// Row-level results of a [`GhosttyTerminal::read_screen_row_visit`] walk —
/// everything about the row that is not a per-cell callback.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScreenRowMeta {
    /// This row soft-wraps into the next one (logical-line join point).
    pub wrapped: bool,
    /// OSC 133 `;A` tag: this row starts a prompt (harvest attribution anchor).
    pub prompt_start: bool,
    /// This row holds a kitty unicode placeholder.
    pub virtual_placeholder: bool,
    /// OSC 8 spans: `(start_col, end_col_inclusive, uri)`.
    pub hyperlinks: Vec<(u16, u16, String)>,
}

/// A materialized styled row read — test-only convenience over the visitor.
/// `cells` follows the snapshot convention: sparse (blank default cells are
/// skipped), ascending `x`, with `y` fixed at 0 (row-local).
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenRowRead {
    pub cells: Vec<RowCell>,
    /// This row soft-wraps into the next one (logical-line join point).
    pub wrapped: bool,
    /// OSC 133 `;A` tag: this row starts a prompt (harvest attribution anchor).
    pub prompt_start: bool,
    /// OSC 8 spans: `(start_col, end_col_inclusive, uri)`.
    pub hyperlinks: Vec<(u16, u16, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotCursor {
    pub x: u16,
    pub y: u16,
    /// `true` when the cursor should be shown — DECTCEM on **and** within the
    /// viewport (render-state `CURSOR_VISIBLE` ∧ `CURSOR_VIEWPORT_HAS_VALUE`).
    pub visible: bool,
    /// DECSCUSR shape from the render-state `CURSOR_VISUAL_STYLE`.
    pub shape: ansi::CursorShape,
    /// Modes-based blink from the render-state `CURSOR_BLINKING`.
    pub blinking: bool,
}

/// The terminal's effective default colors from the render state:
/// `fg`/`bg` (OSC 10/11, always present) and `cursor` (OSC 12, only when set).
/// These become the `term_colors` OSC-override layer over the config palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SnapshotColors {
    pub fg: ColorRgb,
    pub bg: ColorRgb,
    pub cursor: Option<ColorRgb>,
    /// The **effective** window background (terminal-level `COLOR_BACKGROUND`): an
    /// OSC 11 override, or the config default pushed at init, or `None` when no bg
    /// is set at all. The renderer compares it to the config default to tell an OSC
    /// 11 override from a reset/default and keep config opacity/image.
    pub bg_override: Option<ColorRgb>,
}

/// A kitty-graphics placement captured from the engine. Positions are
/// **viewport-relative** (`placement_viewport_pos` already did scroll/cull), so the
/// renderer uses them directly — no `dest_row − (history_size − display_offset)`.
/// Virtual placements (unicode placeholders) carry only `image_id`/`is_virtual`
/// (the engine returns no position for them); terminal positions those from the
/// placeholder cells instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotPlacement {
    pub image_id: u32,
    /// Kitty placement ID. Distinguishes multiple placements of one image; needed
    /// to associate virtual-placeholder runs with the right placement.
    pub placement_id: u32,
    pub is_virtual: bool,
    /// Viewport-relative top-left. `row` may be negative (scrolled partly above).
    pub viewport_col: i32,
    pub viewport_row: i32,
    /// Rendered pixel size of the placement.
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// Grid cells the placement spans.
    pub grid_cols: u32,
    pub grid_rows: u32,
    /// Sub-cell pixel offsets (kitty `X=`/`Y=`).
    pub cell_x_offset: u32,
    pub cell_y_offset: u32,
    /// Resolved source rectangle in image pixels (kitty `x=`/`y=`/`w=`/`h=`).
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub z: i32,
}

/// Geometry of one non-virtual kitty placement in absolute rows.
/// For frozen blocks ([`GhosttyTerminal::block_placements`]) the rows are
/// block-relative — the same row space `BlockRef::read_row_visit` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementScreenPos {
    pub image_id: u32,
    pub placement_id: u32,
    /// Absolute SCREEN column/row of the placement's top-left pin.
    pub screen_col: u32,
    pub screen_row: u32,
    pub grid_cols: u32,
    pub grid_rows: u32,
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub z: i32,
}

/// Engine scrollbar geometry (a terminal-side `Eq` mirror of the FFI
/// `TerminalScrollbar`): `total` scrollable rows, `offset` of the viewport top in
/// that area (top-anchored, `0..total-len`), `len` visible rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScrollbarInfo {
    pub total: u64,
    pub offset: u64,
    pub len: u64,
}
