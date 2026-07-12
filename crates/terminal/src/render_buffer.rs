//! Lightweight render grid fed from the Ghostty engine.
//!
//! A decoupled copy of the visible viewport that the GPUI terminal pane reads
//! instead of the Ghostty render-state directly. It holds `Vec<Row<Square>>`, an
//! interned `style_table`, a grapheme `extras` map, cursor state, colors,
//! scrollbar geometry, and terminal-graphics placement metadata.
//!
//! The **PTY thread** populates it from Ghostty snapshots; the GPUI frontend
//! extracts `TerminalFrame`s from it. It lives behind its own lock, separate from
//! the engine lock, so normal frame extraction does not wait behind `write_vt`
//! parsing.
//!
//! The frontend reads this copy without holding the engine lock during paint.

use nmt_config::colors::{AnsiColor, ColorRgb, NamedColor};
use rustc_hash::FxHashMap;

use crate::ghostty::{
    CellWide, Color, ScrollbarInfo, SnapshotPlacement, SnapshotStyle, TerminalSnapshot, Underline,
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
            .map(to_ansi)
            .unwrap_or(AnsiColor::Named(NamedColor::Foreground)),
        bg: s
            .bg
            .map(to_ansi)
            .unwrap_or(AnsiColor::Named(NamedColor::Background)),
        underline_color: s.underline_color.map(to_ansi),
        flags,
    }
}

pub(crate) fn to_ansi(c: Color) -> AnsiColor {
    AnsiColor::Spec(ColorRgb {
        r: c.r,
        g: c.g,
        b: c.b,
    })
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
    cursor: Pos,
    cursor_visible: bool,
    /// DECSCUSR shape + modes-based blink captured from the engine render-state.
    cursor_shape: crate::ansi::CursorShape,
    cursor_blinking: bool,
    /// Effective default colors captured from the render-state: the
    /// `term_colors` OSC-override layer (Foreground/Background/Cursor) over the
    /// renderer's config palette. Other slots stay `None` (config fallback).
    colors: nmt_config::colors::term::TermColors,
    /// OSC 11 window-background override: `Some` only when a program
    /// explicitly set it, so the renderer falls back to the config window bg /
    /// opacity otherwise.
    window_bg_override: Option<nmt_config::colors::ColorRgb>,
    /// Engine scrollbar geometry captured with this snapshot. The frontend
    /// draws the scrollbar from here, avoiding a per-frame engine read.
    scrollbar: ScrollbarInfo,
    /// Kitty-graphics placements captured from the engine. The current GPUI
    /// frontend keeps this metadata available but does not paint inline images yet.
    placements: Vec<SnapshotPlacement>,
    /// New PTY/render content since the frontend last consumed it. Set by every
    /// `update()`, cleared by `take_content_changed()`. Starts true so the first
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
            cursor: Pos::default(),
            cursor_visible: false,
            cursor_shape: crate::ansi::CursorShape::Block,
            cursor_blinking: false,
            colors: nmt_config::colors::term::TermColors::default(),
            window_bg_override: None,
            scrollbar: ScrollbarInfo::default(),
            placements: Vec::new(),
            content_changed: true,
        }
    }

    /// Consume the "new PTY content since last frame" flag.
    /// Returns whether `update()` ran since the previous call, then clears it.
    /// The frontend uses `true` to invalidate its cached terminal frame.
    pub fn take_content_changed(&mut self) -> bool {
        std::mem::replace(&mut self.content_changed, false)
    }

    /// Kitty-graphics placements captured this snapshot.
    pub fn placements(&self) -> &[SnapshotPlacement] {
        &self.placements
    }

    /// The `term_colors` OSC-override layer captured from the render-state. The
    /// frontend falls back to its config palette for unset slots.
    pub fn colors(&self) -> nmt_config::colors::term::TermColors {
        self.colors
    }

    /// The OSC 11 window-background override, or `None` for the
    /// config window bg / opacity.
    pub fn window_bg_override(&self) -> Option<nmt_config::colors::ColorRgb> {
        self.window_bg_override
    }

    /// The cursor's DECSCUSR shape captured from the render-state.
    pub fn cursor_shape(&self) -> crate::ansi::CursorShape {
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

    /// Repopulate the buffer from a Ghostty snapshot. Auto-resizes to the
    /// snapshot dimensions, so it follows the engine on resize with no extra
    /// plumbing.
    pub fn update(&mut self, snap: &TerminalSnapshot) {
        let cols = (snap.cols as usize).max(1);
        let rows = snap.rows as usize;
        if cols != self.cols || rows != self.rows {
            self.cols = cols;
            self.rows = rows;
            self.grid = (0..rows).map(|_| Row::new(cols)).collect();
        } else {
            for row in self.grid.iter_mut() {
                for sq in row.inner.iter_mut() {
                    *sq = Square::default();
                }
                row.has_extras = false;
                row.dirty = true;
            }
        }
        self.extras.clear();
        self.next_extras_id = 1;

        // Per-row soft-wrap flags (clamped/padded to `rows`).
        self.row_wrapped.clear();
        self.row_wrapped.resize(rows, false);
        for (y, &w) in snap.row_wrapped.iter().take(rows).enumerate() {
            self.row_wrapped[y] = w;
        }

        // Per-row kitty virtual-placeholder flag. Set every row each
        // update (the dims-match clear path above does not reset it).
        for (y, row) in self.grid.iter_mut().enumerate() {
            row.kitty_virtual_placeholder = snap
                .row_virtual_placeholder
                .get(y)
                .copied()
                .unwrap_or(false);
        }

        self.placements = snap.placements.clone();

        for cell in &snap.cells {
            let (x, y) = (cell.x as usize, cell.y as usize);
            if x >= self.cols || y >= self.rows {
                continue;
            }
            let id = self.styles.intern(style_from_snapshot(&cell.style));
            let mut chars = cell.text.chars();
            let base = chars.next().unwrap_or(' ');

            let sq = &mut self.grid[y][Column(x)];
            sq.set_c(base);
            sq.set_style_id(id);
            sq.set_wide(wide_from(cell.wide));

            // Full grapheme fidelity: trailing codepoints (combining marks, ZWJ
            // joiners) become an `Extras { zerowidth }` entry so the shaper sees
            // the whole cluster, not just the base codepoint.
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
                self.grid[y].has_extras = true;
            }
        }

        let cx = (snap.cursor.x as usize).min(self.cols.saturating_sub(1));
        let cy = (snap.cursor.y as usize).min(self.rows.saturating_sub(1));
        self.cursor = Pos::new(Line(cy as i32), Column(cx));
        self.cursor_visible = snap.cursor.visible;
        self.cursor_shape = snap.cursor.shape;
        self.cursor_blinking = snap.cursor.blinking;
        {
            use nmt_config::colors::NamedColor;
            let mut tc = nmt_config::colors::term::TermColors::default();
            tc[NamedColor::Foreground] = Some(snap.colors.fg.to_arr());
            tc[NamedColor::Background] = Some(snap.colors.bg.to_arr());
            if let Some(c) = snap.colors.cursor {
                tc[NamedColor::Cursor] = Some(c.to_arr());
            }
            self.colors = tc;
        }
        self.window_bg_override = snap.colors.bg_override;
        self.scrollbar = snap.scrollbar;
        // The renderer consumes new-content damage via
        // `take_content_changed()` instead of the mirror's `peek_damage_event()`.
        self.content_changed = true;
    }
}
