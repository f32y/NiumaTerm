use std::{os, ptr};

use libghostty_vt_sys::{
    Cell as VtCell, CellContentTag as VtCellContentTag, CellData as VtCellData,
    CellWide as VtCellWide, ColorPaletteIndex as VtColorPaletteIndex, ColorRgb as VtColorRgb,
    GridRef as VtGridRef, Point as VtPoint, PointCoordinate as VtPointCoordinate,
    PointTag as VtPointTag, PointValue as VtPointValue, Result as VtResult, Row as VtRow,
    RowData as VtRowData, RowSemanticPrompt as VtRowSemanticPrompt, Style as VtStyle,
    StyleColor as VtStyleColor, StyleColorTag as VtStyleColorTag, ghostty_cell_get,
    ghostty_cell_get_multi, ghostty_grid_ref_cell, ghostty_grid_ref_graphemes,
    ghostty_grid_ref_hyperlink_uri, ghostty_grid_ref_row, ghostty_grid_ref_style,
    ghostty_row_get_multi, ghostty_terminal_grid_ref, sized as vt_sized,
};

use crate::ghostty::types::color_from_vt;
use crate::ghostty::{
    CellText, CellWide, Color, Error, GhosttyTerminal, Result, ScreenRowMeta, SnapshotStyle,
    Underline,
};
#[cfg(test)]
use crate::ghostty::{RowCell, ScreenRowRead};

impl GhosttyTerminal {
    /// Resolve a point (in the given coordinate system) to a `GridRef`. Fast for
    /// `VIEWPORT`/`ACTIVE`; **O(scrollback) for `SCREEN`/`HISTORY`**. The ref is
    /// valid only until the next mutating call (`write_vt`/`resize`/
    /// `scroll_viewport`) — use it within one read pass, never cache it.
    pub fn grid_ref_at(&self, tag: VtPointTag::Type, x: u16, y: u32) -> Result<VtGridRef> {
        let point = VtPoint {
            tag,
            value: VtPointValue {
                coordinate: VtPointCoordinate { x, y },
            },
        };

        let mut grid_ref = VtGridRef::default();

        Error::from_code(unsafe {
            ghostty_terminal_grid_ref(self.terminal, point, &mut grid_ref)
        })?;

        Ok(grid_ref)
    }

    /// Resolve a viewport coordinate to a `GridRef` (fast).
    pub fn viewport_grid_ref(&self, x: u16, y: u16) -> Result<VtGridRef> {
        self.grid_ref_at(VtPointTag::VIEWPORT, x, y as u32)
    }

    /// The SCREEN row of the top visible row (`viewport_top`) — the constant that
    /// maps between SCREEN and visible coordinates (`screen_row = viewport_top +
    /// visible_row`). One cheap viewport `grid_ref`; `None` if the viewport is
    /// empty. Selection rendering uses this to translate coordinate spaces.
    pub fn viewport_top_screen(&self) -> Option<u32> {
        let r = self.viewport_grid_ref(0, 0).ok()?;

        self.point_from_grid_ref(&r, VtPointTag::SCREEN)
            .ok()
            .flatten()
            .map(|(_, y)| y)
    }

    /// Read one absolute `SCREEN` row into a materialized `Vec` — test-only
    /// convenience over [`Self::read_screen_row_visit`].
    #[cfg(test)]
    pub fn read_screen_row(&self, row: u32) -> Result<Option<ScreenRowRead>> {
        let mut cells = Vec::with_capacity(self.cols as usize);

        let meta =
            self.read_screen_row_visit(row, &self.color_palette(), |x, text, wide, style| {
                cells.push(RowCell {
                    x,
                    text,
                    wide,
                    style,
                })
            })?;

        Ok(meta.map(|meta| ScreenRowRead {
            cells,
            wrapped: meta.wrapped,
            prompt_start: meta.prompt_start,
            hyperlinks: meta.hyperlinks,
        }))
    }

    /// Walk one absolute `SCREEN` row with styles, invoking `on_cell` for each
    /// content cell (sparse: blank default cells are skipped) instead of
    /// materializing a `Vec` — the harvester constructs its `LineCell`s in
    /// place, so no intermediate row buffer exists on the freeze hot path.
    /// Colors resolve against a caller-supplied palette (hoisted out of
    /// per-row cost: the palette is a 256-entry FFI copy and cannot change
    /// while the engine lock is held). Reaches any scrollback row without
    /// moving the viewport or refreshing the render state. Returns `None`
    /// when `row` is out of range.
    ///
    /// The pin lookup is O(scrollback page hops); per-cell reads are O(cols).
    /// The `GridRef`s are created and dropped within this call so mutations cannot
    /// invalidate a cached reference.
    /// Per-cell FFI is tag-driven: blank/plain-codepoint cells never touch the
    /// grapheme or style readers, keeping the row-harvest hot path free of unnecessary FFI.
    pub fn read_screen_row_visit(
        &self,
        row: u32,
        palette: &[VtColorRgb; 256],
        on_cell: impl FnMut(u16, CellText, CellWide, SnapshotStyle),
    ) -> Result<Option<ScreenRowMeta>> {
        let grid_ref = match self.grid_ref_at(VtPointTag::SCREEN, 0, row) {
            Ok(r) => r,
            Err(Error::InvalidValue) => return Ok(None),
            Err(e) => return Err(e),
        };

        Ok(Some(Self::visit_row_cells(
            grid_ref, self.cols, palette, on_cell,
        )?))
    }

    /// Shared per-row cell walk over a resolved row `GridRef` — the body of
    /// [`Self::read_screen_row_visit`], also used by finished-block reads
    /// ([`Self::read_block_row_visit`]) where the ref comes from the block
    /// resolver instead of the active screen.
    pub(super) fn visit_row_cells(
        mut grid_ref: VtGridRef,
        cols: u16,
        palette: &[VtColorRgb; 256],
        mut on_cell: impl FnMut(u16, CellText, CellWide, SnapshotStyle),
    ) -> Result<ScreenRowMeta> {
        // Row flags from the raw row handle (same keys the snapshot path
        // reads), fetched in one multi-get FFI call.
        let mut raw_row: VtRow = 0;

        Error::from_code(unsafe { ghostty_grid_ref_row(&grid_ref, &mut raw_row) })?;

        let mut wrapped = false;
        let mut prompt_tag: VtRowSemanticPrompt::Type = VtRowSemanticPrompt::NONE;
        let mut has_link = false;
        let mut virtual_placeholder = false;

        {
            const ROW_KEYS: [VtRowData::Type; 4] = [
                VtRowData::WRAP,
                VtRowData::SEMANTIC_PROMPT,
                VtRowData::HYPERLINK,
                VtRowData::KITTY_VIRTUAL_PLACEHOLDER,
            ];

            let mut values: [*mut os::raw::c_void; 4] = [
                (&mut wrapped as *mut bool).cast(),
                (&mut prompt_tag as *mut VtRowSemanticPrompt::Type).cast(),
                (&mut has_link as *mut bool).cast(),
                (&mut virtual_placeholder as *mut bool).cast(),
            ];

            unsafe {
                let _ = ghostty_row_get_multi(
                    raw_row,
                    ROW_KEYS.len(),
                    ROW_KEYS.as_ptr(),
                    values.as_mut_ptr(),
                    ptr::null_mut(),
                );
            }
        }

        let mut hyperlinks: Vec<(u16, u16, String)> = Vec::new();

        for x in 0..cols {
            // All cells of one row share the pin's node; stepping `x` in place
            // avoids re-resolving the O(scrollback) SCREEN pin per cell.
            grid_ref.x = x;

            let mut raw: VtCell = 0;

            if unsafe { ghostty_grid_ref_cell(&grid_ref, &mut raw) } != VtResult::SUCCESS {
                continue;
            }

            // Tag-driven per-cell reads on the raw cell handle, fetched in one
            // multi-get FFI call: the common cases (blank, single codepoint)
            // never call the grapheme reader. CODEPOINT is deliberately last —
            // multi-get stops at the first error, and a bg-color-only cell that
            // rejected it would still have tag/wide/styling written while `cp`
            // keeps its correct 0 default.
            let mut tag: VtCellContentTag::Type = VtCellContentTag::CODEPOINT;
            let mut wide_raw: VtCellWide::Type = VtCellWide::NARROW;
            let mut has_styling = false;
            let mut cp: u32 = 0;

            {
                const CELL_KEYS: [VtCellData::Type; 4] = [
                    VtCellData::CONTENT_TAG,
                    VtCellData::WIDE,
                    VtCellData::HAS_STYLING,
                    VtCellData::CODEPOINT,
                ];
                let mut values: [*mut os::raw::c_void; 4] = [
                    (&mut tag as *mut i32).cast(),
                    (&mut wide_raw as *mut i32).cast(),
                    (&mut has_styling as *mut bool).cast(),
                    (&mut cp as *mut u32).cast(),
                ];

                unsafe {
                    let _ = ghostty_cell_get_multi(
                        raw,
                        CELL_KEYS.len(),
                        CELL_KEYS.as_ptr(),
                        values.as_mut_ptr(),
                        ptr::null_mut(),
                    );
                }
            }

            let wide = CellWide::from(wide_raw);

            let text = match tag {
                VtCellContentTag::CODEPOINT => {
                    if cp == 0 {
                        CellText::default()
                    } else {
                        CellText::from_char(
                            char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER),
                        )
                    }
                }
                VtCellContentTag::CODEPOINT_GRAPHEME => {
                    CellText::from(grid_ref_graphemes(&grid_ref))
                }
                _ => CellText::default(), // BG_COLOR_*: no text
            };

            // The style struct read + resolve only runs for cells the engine
            // flags as styled; default-styled text (the bulk of scroll floods)
            // skips it entirely.
            let mut style = SnapshotStyle::default();

            if has_styling {
                let mut raw_style = vt_sized!(VtStyle);

                if unsafe { ghostty_grid_ref_style(&grid_ref, &mut raw_style) } == VtResult::SUCCESS
                {
                    style.fg = style_color_resolve(&raw_style.fg_color, palette);
                    style.bg = style_color_resolve(&raw_style.bg_color, palette);
                    style.underline_color =
                        style_color_resolve(&raw_style.underline_color, palette);
                    style.bold = raw_style.bold;
                    style.italic = raw_style.italic;
                    style.faint = raw_style.faint;
                    style.blink = raw_style.blink;
                    style.inverse = raw_style.inverse;
                    style.invisible = raw_style.invisible;
                    style.strikethrough = raw_style.strikethrough;
                    style.overline = raw_style.overline;
                    style.underline = Underline::from(raw_style.underline);
                }
            }

            // Erased-with-bg cells carry their color in the content tag, not the
            // style (mirrors the render-state BG_COLOR resolution).
            if style.bg.is_none() {
                if tag == VtCellContentTag::BG_COLOR_PALETTE {
                    let mut idx: VtColorPaletteIndex = 0;

                    if unsafe {
                        ghostty_cell_get(
                            raw,
                            VtCellData::COLOR_PALETTE,
                            (&mut idx as *mut VtColorPaletteIndex).cast(),
                        )
                    } == VtResult::SUCCESS
                    {
                        style.bg = Some(color_from_vt(palette[idx as usize]));
                    }
                } else if tag == VtCellContentTag::BG_COLOR_RGB {
                    let mut rgb = VtColorRgb::default();

                    if unsafe {
                        ghostty_cell_get(
                            raw,
                            VtCellData::COLOR_RGB,
                            (&mut rgb as *mut VtColorRgb).cast(),
                        )
                    } == VtResult::SUCCESS
                    {
                        style.bg = Some(color_from_vt(rgb));
                    }
                }
            }

            if has_link {
                if let Some(uri) = grid_ref_hyperlink_uri(&grid_ref) {
                    match hyperlinks.last_mut() {
                        Some((_, end, last_uri)) if *end + 1 == x && *last_uri == uri => *end = x,
                        _ => hyperlinks.push((x, x, uri)),
                    }
                }
            }

            if text.is_empty() && style.bg.is_none() && wide == CellWide::Narrow {
                continue;
            }

            on_cell(x, text, wide, style);
        }

        Ok(ScreenRowMeta {
            wrapped,
            prompt_start: prompt_tag == VtRowSemanticPrompt::PROMPT,
            virtual_placeholder,
            hyperlinks,
        })
    }
}

/// Resolve a tagged style color against the palette. `None` for the default
/// (terminal-level) color, concrete RGB otherwise.
fn style_color_resolve(c: &VtStyleColor, palette: &[VtColorRgb; 256]) -> Option<Color> {
    match c.tag {
        VtStyleColorTag::PALETTE => {
            let idx = unsafe { c.value.palette } as usize;
            palette.get(idx).map(|&rgb| color_from_vt(rgb))
        }
        VtStyleColorTag::RGB => Some(color_from_vt(unsafe { c.value.rgb })),
        _ => None,
    }
}

/// Read the full grapheme cluster of a `GridRef` cell as a `String`. Empty for
/// blank cells. Stack buffer first; falls back to a heap read for oversized
/// clusters (same two-call pattern as `grid_ref_hyperlink_uri`).
fn grid_ref_graphemes(r: &VtGridRef) -> String {
    fn to_string(codepoints: &[u32]) -> String {
        codepoints
            .iter()
            .map(|&cp| char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    }

    let mut buf = [0u32; 8];
    let mut len: usize = 0;

    match unsafe { ghostty_grid_ref_graphemes(r, buf.as_mut_ptr(), buf.len(), &mut len) } {
        VtResult::SUCCESS => to_string(&buf[..len]),
        VtResult::OUT_OF_SPACE => {
            let mut big = vec![0u32; len];

            match unsafe { ghostty_grid_ref_graphemes(r, big.as_mut_ptr(), big.len(), &mut len) } {
                VtResult::SUCCESS => to_string(&big[..len]),
                _ => String::new(),
            }
        }
        _ => String::new(),
    }
}

/// Read the OSC 8 hyperlink URI for a resolved `GridRef`, or `None` if the cell
/// has none. Two-call pattern: a NULL probe yields the required length (`out_len`
/// is 0 ⇒ no hyperlink), then a sized read.
fn grid_ref_hyperlink_uri(r: &VtGridRef) -> Option<String> {
    let mut len: usize = 0;

    unsafe {
        ghostty_grid_ref_hyperlink_uri(r, ptr::null_mut(), 0, &mut len);
    }

    if len == 0 {
        return None;
    }

    let mut buf = vec![0u8; len];

    let rc = unsafe { ghostty_grid_ref_hyperlink_uri(r, buf.as_mut_ptr(), buf.len(), &mut len) };

    if rc != VtResult::SUCCESS {
        return None;
    }

    buf.truncate(len);

    String::from_utf8(buf).ok()
}
