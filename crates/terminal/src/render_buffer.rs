//! Lightweight render grid fed from the Ghostty engine.
//!
//! A decoupled copy of the visible viewport that the GPUI terminal pane reads
//! instead of the Ghostty render-state directly. It holds `Vec<Row<Square>>`, an
//! interned `style_table`, a grapheme `extras` map, cursor state, colors,
//! scrollbar geometry, and terminal-graphics placement metadata.
//!
//! The **PTY thread** populates it directly from Ghostty; the GPUI frontend
//! extracts `TerminalFrame`s from it. It lives behind its own lock, separate from
//! the engine lock, so normal frame extraction does not wait behind `write_vt`
//! parsing.
//!
//! The frontend reads this copy without holding the engine lock during paint.

use std::mem;

use nmt_config::colors::term::TermColors;
use nmt_config::colors::{AnsiColor, ColorRgb, NamedColor};
use rustc_hash::FxHashMap;

use crate::ansi;
use crate::ghostty::{
    CellWide, ScrollbarInfo, SnapshotColors, SnapshotCursor, SnapshotPlacement, SnapshotStyle,
    Underline,
};
use crate::terminal::grid::row::Row;
use crate::terminal::pos::{Column, Line, Pos};
use crate::terminal::square::{Extras, Square, Wide};
use crate::terminal::style::{Style, StyleFlags, StyleId, StyleSet};

/// Build a `Style` from a Ghostty snapshot style. Ghostty `blink` and
/// `overline` have no render flag and are intentionally dropped rather than
/// synthesizing unsupported styling.
pub(crate) fn style_from_snapshot(s: &SnapshotStyle) -> Style {
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

pub(crate) fn wide_from(w: CellWide) -> Wide {
    match w {
        CellWide::Narrow => Wide::Narrow,
        CellWide::Wide => Wide::Wide,
        CellWide::SpacerTail => Wide::Spacer,
        CellWide::SpacerHead => Wide::LeadingSpacer,
    }
}

/// A decoupled, renderable copy of the visible viewport.
pub struct RenderBuffer {
    cols: usize,
    rows: usize,
    /// One `Row<Square>` per visible line. The GPUI app extracts terminal frames
    /// from these rows.
    grid: Vec<Row<Square>>,
    /// Persistent interner; `styles()` is the `style_table` indexed by style id.
    styles: StyleSet,
    /// Grapheme clusters: a cell's trailing (combining/ZWJ) codepoints, keyed by
    /// the cell's `extras_id`.
    extras: FxHashMap<u16, Extras>,
    /// Buffer-local `extras_id` allocator (1-based; 0 means "no extras").
    next_extras_id: u16,
    /// Per row: `true` when the row soft-wraps into the next. Used by line
    /// selection (`row_search`) to span a wrapped logical line. Length == `rows`.
    row_wrapped: Vec<bool>,
    /// Monotonic engine content version for each visible row. Unlike transient
    /// dirty flags, these survive skipped publications until the UI observes them.
    row_versions: Vec<u64>,
    cursor: Pos,
    cursor_visible: bool,
    /// DECSCUSR shape + modes-based blink captured from the engine render-state.
    cursor_shape: ansi::CursorShape,
    cursor_blinking: bool,
    /// Effective default colors captured from the render-state: the
    /// `term_colors` OSC-override layer (Foreground/Background/Cursor) over the
    /// renderer's config palette. Other slots stay `None` (config fallback).
    colors: TermColors,
    /// OSC 11 window-background override: `Some` only when a program
    /// explicitly set it, so the renderer falls back to the config window bg /
    /// opacity otherwise.
    window_bg_override: Option<ColorRgb>,
    /// Engine scrollbar geometry captured with this snapshot. The frontend
    /// draws the scrollbar from here, avoiding a per-frame engine read.
    scrollbar: ScrollbarInfo,
    /// Kitty-graphics placements captured from the engine. The current GPUI
    /// frontend keeps this metadata available but does not paint inline images yet.
    placements: Vec<SnapshotPlacement>,
    /// New PTY/render content since the frontend last consumed it. Set by every
    /// capture, cleared by `take_content_changed()`. Starts true so the first
    /// frame builds from the freshly initialized buffer.
    content_changed: bool,
}

impl RenderBuffer {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            grid: (0..rows).map(|_| Row::new(cols.max(1))).collect(),
            styles: StyleSet::new(),
            extras: FxHashMap::default(),
            next_extras_id: 1,
            row_wrapped: vec![false; rows],
            row_versions: vec![0; rows],
            cursor: Pos::default(),
            cursor_visible: false,
            cursor_shape: ansi::CursorShape::Block,
            cursor_blinking: false,
            colors: TermColors::default(),
            window_bg_override: None,
            scrollbar: ScrollbarInfo::default(),
            placements: Vec::new(),
            content_changed: true,
        }
    }

    /// Consume the "new PTY content since last frame" flag.
    /// Returns whether capture ran since the previous call, then clears it.
    /// The frontend uses `true` to invalidate its cached terminal frame.
    pub fn take_content_changed(&mut self) -> bool {
        mem::replace(&mut self.content_changed, false)
    }

    /// Kitty-graphics placements captured this snapshot.
    pub fn placements(&self) -> &[SnapshotPlacement] {
        &self.placements
    }

    /// The `term_colors` OSC-override layer captured from the render-state. The
    /// frontend falls back to its config palette for unset slots.
    pub fn colors(&self) -> TermColors {
        self.colors
    }

    /// The OSC 11 window-background override, or `None` for the
    /// config window bg / opacity.
    pub fn window_bg_override(&self) -> Option<ColorRgb> {
        self.window_bg_override
    }

    /// The cursor's DECSCUSR shape captured from the render-state.
    pub fn cursor_shape(&self) -> ansi::CursorShape {
        self.cursor_shape
    }

    /// Whether the cursor blinks, derived from render-state modes.
    pub fn cursor_blinking(&self) -> bool {
        self.cursor_blinking
    }

    /// The engine scrollbar geometry captured with the last snapshot.
    pub fn scrollbar(&self) -> ScrollbarInfo {
        self.scrollbar
    }

    /// Whether visible row `y` soft-wraps into the next row.
    pub fn row_wrapped(&self, y: usize) -> bool {
        self.row_wrapped.get(y).copied().unwrap_or(false)
    }

    /// Per-row soft-wrap flags (length == `rows`), for the selection searches.
    pub fn row_wrapped_all(&self) -> &[bool] {
        &self.row_wrapped
    }

    /// Last engine content version for each visible row.
    pub fn row_versions(&self) -> &[u64] {
        &self.row_versions
    }

    /// Whether visible row `y` holds any kitty unicode-placeholder cells.
    /// Guards the frame path's virtual-placement decode fast path.
    pub fn row_has_virtual_placeholder(&self, y: usize) -> bool {
        self.grid
            .get(y)
            .is_some_and(|row| row.kitty_virtual_placeholder)
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cursor(&self) -> Pos {
        self.cursor
    }

    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// The cell at `(x, y)`. Returns the default cell when out of bounds.
    pub fn cell(&self, x: usize, y: usize) -> Square {
        if x >= self.cols || y >= self.rows {
            return Square::default();
        }
        self.grid[y][Column(x)]
    }

    /// The visible rows, for the renderer's per-frame copy.
    pub fn grid(&self) -> &[Row<Square>] {
        &self.grid
    }

    /// The interned style table, indexed by a cell's `style_id`.
    pub fn style_table(&self) -> &[Style] {
        self.styles.styles()
    }

    /// The per-frame grapheme extras, keyed by a cell's `extras_id`.
    pub fn extras(&self) -> &FxHashMap<u16, Extras> {
        &self.extras
    }

    /// Resolve a cell's interned style.
    pub fn style(&self, id: StyleId) -> Style {
        self.styles.get(id)
    }

    pub(crate) fn begin_capture(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);

        if cols != self.cols || rows != self.rows {
            self.cols = cols;
            self.rows = rows;
            self.grid = (0..rows).map(|_| Row::new(cols)).collect();
        } else {
            for row in &mut self.grid {
                row.inner.fill(Square::default());
                row.kitty_virtual_placeholder = false;
            }
        }

        self.extras.clear();
        self.next_extras_id = 1;
        self.row_wrapped.clear();
        self.row_wrapped.resize(rows, false);
        self.placements.clear();

        // Every capture rewrites every visible cell (the grid was just
        // cleared above) and re-interns each cell's style, so no style id
        // survives into the next capture. Resetting the interner here bounds
        // it to one grid's worth of distinct styles; a persistent interner
        // grows monotonically under truecolor-gradient output until it
        // saturates at u16::MAX, after which every new style silently renders
        // as the default style.
        self.styles.clear();
    }

    pub(crate) fn write_cell(
        &mut self,
        x: usize,
        y: usize,
        text: &str,
        wide: CellWide,
        style: &SnapshotStyle,
    ) {
        if x >= self.cols || y >= self.rows {
            return;
        }

        let id = self.styles.intern(style_from_snapshot(style));

        let mut chars = text.chars();

        let base = chars.next().unwrap_or(' ');
        let sq = &mut self.grid[y][Column(x)];

        sq.set_c(base);
        sq.set_style_id(id);
        sq.set_wide(wide_from(wide));

        let zerowidth: Vec<char> = chars.collect();

        if !zerowidth.is_empty() {
            let extras_id = self.next_extras_id;

            self.next_extras_id = self.next_extras_id.saturating_add(1);

            self.extras.insert(
                extras_id,
                Extras {
                    zerowidth,
                    ..Extras::default()
                },
            );

            sq.set_extras_id(Some(extras_id));
        }
    }

    pub(crate) fn write_row_meta(&mut self, y: usize, wrapped: bool, placeholder: bool) {
        if let Some(value) = self.row_wrapped.get_mut(y) {
            *value = wrapped;
        }

        if let Some(row) = self.grid.get_mut(y) {
            row.kitty_virtual_placeholder = placeholder;
        }
    }

    pub(crate) fn finish_capture(
        &mut self,
        cursor: SnapshotCursor,
        colors: SnapshotColors,
        placements: Vec<SnapshotPlacement>,
        scrollbar: ScrollbarInfo,
        row_versions: &[u64],
    ) {
        let cx = (cursor.x as usize).min(self.cols.saturating_sub(1));
        let cy = (cursor.y as usize).min(self.rows.saturating_sub(1));

        self.cursor = Pos::new(Line(cy as i32), Column(cx));
        self.cursor_visible = cursor.visible;
        self.cursor_shape = cursor.shape;
        self.cursor_blinking = cursor.blinking;

        use nmt_config::colors::NamedColor;

        let mut term_colors = TermColors::default();

        term_colors[NamedColor::Foreground] = Some(colors.fg.to_arr());
        term_colors[NamedColor::Background] = Some(colors.bg.to_arr());

        if let Some(color) = colors.cursor {
            term_colors[NamedColor::Cursor] = Some(color.to_arr());
        }

        self.colors = term_colors;
        self.window_bg_override = colors.bg_override;
        self.placements = placements;
        self.scrollbar = scrollbar;
        self.row_versions.clear();
        self.row_versions.extend_from_slice(row_versions);
        self.content_changed = true;
    }

    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.cursor_visible = visible;
    }
}
