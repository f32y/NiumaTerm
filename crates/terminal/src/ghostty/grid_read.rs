use std::{os, ptr};

use libghostty_vt_sys::{
    Cell as VtCell, CellContentTag as VtCellContentTag, CellData as VtCellData,
    CellWide as VtCellWide, ColorPaletteIndex as VtColorPaletteIndex, ColorRgb as VtColorRgb,
    GridRef as VtGridRef, Result as VtResult, Row as VtRow, RowData as VtRowData,
    RowSemanticPrompt as VtRowSemanticPrompt, Style as VtStyle, StyleColor as VtStyleColor,
    StyleColorTag as VtStyleColorTag, ghostty_cell_get, ghostty_cell_get_multi,
    ghostty_grid_ref_cell, ghostty_grid_ref_graphemes, ghostty_grid_ref_hyperlink_uri,
    ghostty_grid_ref_row, ghostty_grid_ref_style, ghostty_row_get_multi, sized as vt_sized,
};

#[cfg(doc)]
use crate::ghostty::GhosttyTerminal;
use crate::ghostty::types::color_from_vt;
use crate::ghostty::{CellText, CellWide, Color, Error, Result, ScreenRowMeta, SnapshotStyle};

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

/// Shared per-row cell walk over a resolved row `GridRef` — the body of
/// [`GhosttyTerminal::read_screen_row_visit`], also used by finished-block reads
/// ([`GhosttyTerminal::read_block_row_visit`]) where the ref comes from the block
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

        let wide: CellWide = wide_raw.into();

        let text = match tag {
            VtCellContentTag::CODEPOINT => {
                if cp == 0 {
                    CellText::default()
                } else {
                    char::from_u32(cp)
                        .unwrap_or(char::REPLACEMENT_CHARACTER)
                        .into()
                }
            }

            VtCellContentTag::CODEPOINT_GRAPHEME => grid_ref_graphemes(&grid_ref).into(),
            _ => CellText::default(), // BG_COLOR_*: no text
        };

        // The style struct read + resolve only runs for cells the engine
        // flags as styled; default-styled text (the bulk of scroll floods)
        // skips it entirely.
        let mut style = SnapshotStyle::default();

        if has_styling {
            let mut raw_style = vt_sized!(VtStyle);

            if unsafe { ghostty_grid_ref_style(&grid_ref, &mut raw_style) } == VtResult::SUCCESS {
                style.fg = style_color_resolve(&raw_style.fg_color, palette);
                style.bg = style_color_resolve(&raw_style.bg_color, palette);
                style.underline_color = style_color_resolve(&raw_style.underline_color, palette);
                style.bold = raw_style.bold;
                style.italic = raw_style.italic;
                style.faint = raw_style.faint;
                style.blink = raw_style.blink;
                style.inverse = raw_style.inverse;
                style.invisible = raw_style.invisible;
                style.strikethrough = raw_style.strikethrough;
                style.overline = raw_style.overline;
                style.underline = raw_style.underline.into();
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

        if has_link && let Some(uri) = grid_ref_hyperlink_uri(&grid_ref) {
            match hyperlinks.last_mut() {
                Some((_, end, last_uri)) if *end + 1 == x && *last_uri == uri => *end = x,
                _ => hyperlinks.push((x, x, uri)),
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
