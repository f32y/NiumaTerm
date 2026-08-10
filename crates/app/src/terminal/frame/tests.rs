use std::sync::Arc;

use nmt_config::colors::term::TermColors;
use nmt_config::colors::{ColorArray, Colors, NamedColor};
use nmt_terminal::ansi::CursorShape;
use nmt_terminal::ansi::kitty_virtual::DIACRITICS;
use nmt_terminal::ghostty::GhosttyTerminal;
use nmt_terminal::render_buffer::RenderBuffer;
use nmt_terminal::selection::SelectionRange;
use nmt_terminal::terminal::pos::{Column, Line, Pos};
use nmt_terminal::terminal::square::Wide;

use crate::terminal;
use crate::terminal::frame::{
    BackgroundColors, FrameImageKind, GenerationMap, TerminalColor, TerminalFrame,
    TerminalFrameCache, ZLayer, cursor_for_row, extract_frame_images, extract_row,
    extract_row_with_colors, frame_cursor, line_from_parts, theme_default_foreground,
};

fn frame_with_line(line: &str) -> TerminalFrame {
    TerminalFrame {
        lines: Arc::from([line_from_parts(line.to_owned(), Vec::new(), Vec::new())]),
        line_states: Arc::from([Default::default()]),
        cols: line.len(),
        cursor: None,
        scrollbar: Default::default(),
        images: Arc::from([]),
    }
}

fn first_line(frame: &TerminalFrame) -> &str {
    frame.lines()[0].text().as_ref()
}

#[test]
fn terminal_cursor_color_prefers_runtime_override() {
    let expected = ColorArray::from([0.8, 0.1, 0.2, 1.0]);
    let mut term_colors = TermColors::default();
    term_colors[NamedColor::Cursor] = Some(expected);

    let colors = BackgroundColors::new(term_colors);

    assert_eq!(
        colors.named(NamedColor::Cursor),
        TerminalColor::from_color_arr(expected)
    );
}

#[test]
fn block_cursor_uses_terminal_background_for_glyph() {
    let mut engine = GhosttyTerminal::new(4, 1, 100).unwrap();
    engine.write_vt(b"A\x1b[D");
    let mut buf = RenderBuffer::new(4, 1);
    engine.snapshot_into(&mut buf).unwrap();

    let gray = |value: u8| {
        let value = f32::from(value) / 255.;
        ColorArray::from([value, value, value, 1.])
    };
    let mut term_colors = TermColors::default();
    term_colors[NamedColor::Foreground] = Some(gray(0x29));
    term_colors[NamedColor::Background] = Some(gray(0xe0));
    term_colors[NamedColor::Cursor] = Some(gray(0x38));
    let colors = BackgroundColors::new(term_colors);
    let cursor = frame_cursor(&buf, &colors).unwrap();
    let row = extract_row_with_colors(&buf, 0, Some(cursor), &colors, None);

    assert_eq!(cursor.shape, CursorShape::Block);
    assert_eq!(row.runs()[0].fg, colors.named(NamedColor::Background));
}

#[test]
fn extracted_rows_have_stable_content_hashes() {
    let mut engine = GhosttyTerminal::new(4, 1, 100).unwrap();
    engine.write_vt(b"ab");
    let mut buf = RenderBuffer::new(4, 1);
    engine.snapshot_into(&mut buf).unwrap();

    let first = extract_row(&buf, 0, None);
    let second = extract_row(&buf, 0, None);
    assert_eq!(first.text_hash(), second.text_hash());

    engine.write_vt(b"c");
    engine.snapshot_into(&mut buf).unwrap();
    let changed = extract_row(&buf, 0, None);
    assert_ne!(first.text_hash(), changed.text_hash());
}

/// Regression (broken-selection bug): invalidation marks the cache for
/// rebuild but keeps serving the last frame, so pointer/IME mapping between
/// a mouse event and the next render still sees the displayed frame instead
/// of an empty offsets table.
#[test]
fn cache_serves_stale_frame_until_rebuilt() {
    let mut cache = TerminalFrameCache::default();
    assert!(cache.needs_rebuild(), "empty cache must rebuild");

    cache.rebuild(frame_with_line("first"));
    assert!(!cache.needs_rebuild());
    assert_eq!(first_line(&cache.current().unwrap()), "first");

    cache.invalidate();
    assert!(cache.needs_rebuild(), "invalidation forces a rebuild");
    assert!(
        cache.reusable_frame().is_some(),
        "ordinary invalidation keeps the frame eligible for line reuse"
    );
    assert_eq!(
        first_line(&cache.current().unwrap()),
        "first",
        "stale frame stays available for pointer mapping"
    );

    cache.rebuild(frame_with_line("second"));
    assert!(!cache.needs_rebuild());
    assert_eq!(first_line(&cache.current().unwrap()), "second");
}

#[test]
fn cache_full_invalidation_retains_frame_but_disables_reuse_once() {
    let mut cache = TerminalFrameCache::default();
    cache.rebuild(frame_with_line("first"));

    cache.invalidate_full();
    assert!(cache.needs_rebuild());
    assert_eq!(first_line(&cache.current().unwrap()), "first");
    assert!(cache.reusable_frame().is_none());

    cache.rebuild(frame_with_line("second"));
    assert!(!cache.needs_rebuild());
    assert_eq!(
        first_line(&cache.reusable_frame().expect("reuse restored")),
        "second"
    );
}

#[test]
fn incremental_extraction_reuses_only_clean_rows() {
    let mut engine = GhosttyTerminal::new(8, 3, 100).unwrap();
    let mut buf = RenderBuffer::new(8, 3);
    engine.write_vt(b"\x1b[2;1H");
    engine.snapshot_into(&mut buf).unwrap();
    let generations = GenerationMap::new();
    let first = TerminalFrame::from_render_buffer_reusing(&buf, None, &generations, None);

    engine.snapshot_into(&mut buf).unwrap();
    let clean = TerminalFrame::from_render_buffer_reusing(&buf, None, &generations, Some(&first));
    assert!(
        first
            .lines()
            .iter()
            .zip(clean.lines())
            .all(|(old, new)| old.ptr_eq(new)),
        "clean capture reuses every line"
    );

    engine.write_vt(b"X");
    engine.snapshot_into(&mut buf).unwrap();
    let changed = TerminalFrame::from_render_buffer_reusing(&buf, None, &generations, Some(&clean));
    assert!(clean.lines()[0].ptr_eq(&changed.lines()[0]));
    assert!(!clean.lines()[1].ptr_eq(&changed.lines()[1]));
    assert!(clean.lines()[2].ptr_eq(&changed.lines()[2]));

    let forced = TerminalFrame::from_render_buffer_reusing(&buf, None, &generations, None);
    assert!(
        changed
            .lines()
            .iter()
            .zip(forced.lines())
            .all(|(old, new)| !old.ptr_eq(new)),
        "no reusable frame forces full line extraction"
    );
}

#[test]
fn cursor_only_change_rebuilds_affected_row() {
    let mut engine = GhosttyTerminal::new(8, 2, 100).unwrap();
    let mut buf = RenderBuffer::new(8, 2);
    engine.write_vt(b"AB");
    engine.snapshot_into(&mut buf).unwrap();
    let generations = GenerationMap::new();
    let first = TerminalFrame::from_render_buffer_reusing(&buf, None, &generations, None);
    let versions = buf.row_versions().to_vec();

    engine.write_vt(b"\r");
    engine.snapshot_into(&mut buf).unwrap();
    assert_eq!(buf.row_versions(), versions, "CR changes only the cursor");
    let moved = TerminalFrame::from_render_buffer_reusing(&buf, None, &generations, Some(&first));

    assert!(!first.lines()[0].ptr_eq(&moved.lines()[0]));
    assert!(first.lines()[1].ptr_eq(&moved.lines()[1]));
}

#[test]
fn selection_changes_rebuild_only_affected_rows() {
    let mut engine = GhosttyTerminal::new(8, 3, 100).unwrap();
    let mut buf = RenderBuffer::new(8, 3);
    engine.write_vt(b"row0\r\nrow1\r\nrow2");
    engine.snapshot_into(&mut buf).unwrap();
    let generations = GenerationMap::new();
    let plain = TerminalFrame::from_render_buffer_reusing(&buf, None, &generations, None);
    let row0 = SelectionRange::new(
        Pos::new(Line(0), Column(0)),
        Pos::new(Line(0), Column(3)),
        false,
    );
    let selected =
        TerminalFrame::from_render_buffer_reusing(&buf, Some(row0), &generations, Some(&plain));
    assert!(!plain.lines()[0].ptr_eq(&selected.lines()[0]));
    assert!(plain.lines()[1].ptr_eq(&selected.lines()[1]));
    assert!(plain.lines()[2].ptr_eq(&selected.lines()[2]));

    let cleared =
        TerminalFrame::from_render_buffer_reusing(&buf, None, &generations, Some(&selected));
    assert!(!selected.lines()[0].ptr_eq(&cleared.lines()[0]));
    assert!(selected.lines()[1].ptr_eq(&cleared.lines()[1]));
    assert!(selected.lines()[2].ptr_eq(&cleared.lines()[2]));
}

#[test]
fn extracts_row_cells_extras_wide_style_and_cursor() {
    let mut engine = GhosttyTerminal::new(8, 1, 100).unwrap();
    engine.write_vt("e\u{0301}中\x1b[1mB\x1b[0m".as_bytes());
    let mut buf = RenderBuffer::new(8, 1);
    engine.snapshot_into(&mut buf).unwrap();
    let frame = TerminalFrame::from_render_buffer(&buf);
    let row = extract_row(&buf, 0, cursor_for_row(frame.cursor(), 0));

    // The wide '中' is followed by a blank placeholder for its second column.
    assert!(row.text().as_ref().starts_with("e\u{0301}中\u{00a0}B"));
    assert_eq!(row.cursor_col(), Some(4));
    assert!(row.cells().iter().any(|cell| cell.has_cursor));

    let e = &row.cells()[0];
    assert_eq!(e.ch, 'e');
    assert_eq!(e.extras, vec!['\u{0301}']);

    let wide = row.cells().iter().find(|cell| cell.ch == '中').unwrap();
    assert_eq!(wide.wide, Wide::Wide);
    assert!(!row.cells().iter().any(|cell| cell.col == 2));

    let bold = row.cells().iter().find(|cell| cell.ch == 'B').unwrap();
    assert_eq!(bold.style_id, buf.cell(bold.col as usize, 0).style_id());
}

#[test]
fn colored_text_yields_distinct_fg_run_and_cache_key() {
    let mut engine = GhosttyTerminal::new(4, 1, 100).unwrap();
    engine.write_vt(b"\x1b[31mAB\x1b[0m");
    let mut buf = RenderBuffer::new(4, 1);
    engine.snapshot_into(&mut buf).unwrap();
    let colored = extract_row(&buf, 0, None);

    let mut plain_engine = GhosttyTerminal::new(4, 1, 100).unwrap();
    plain_engine.write_vt(b"AB");
    let mut plain_buf = RenderBuffer::new(4, 1);
    plain_engine.snapshot_into(&mut plain_buf).unwrap();
    let plain = extract_row(&plain_buf, 0, None);

    // Identical visible text...
    assert_eq!(colored.text(), plain.text());
    // ...but the red run makes the shape-cache key differ (no stale glyph reuse)...
    assert_ne!(colored.text_hash(), plain.text_hash());
    // ...and a distinct foreground run exists for the colored cells.
    let default_fg = plain.runs()[0].fg;
    assert!(colored.runs().iter().any(|run| run.fg != default_fg));
}

#[test]
fn extracts_cell_backgrounds_from_rgb_style() {
    let mut engine = GhosttyTerminal::new(4, 1, 100).unwrap();
    engine.write_vt(b"\x1b[48;2;1;2;3mA");
    let mut buf = RenderBuffer::new(4, 1);
    engine.snapshot_into(&mut buf).unwrap();

    let row = extract_row(&buf, 0, None);

    assert_eq!(row.cells()[0].background, Some((1, 2, 3).into()));
}

#[test]
fn dim_does_not_change_explicit_background() {
    let mut engine = GhosttyTerminal::new(4, 1, 100).unwrap();
    engine.write_vt(b"\x1b[48;2;120;100;80mA\x1b[2mB");
    let mut buf = RenderBuffer::new(4, 1);
    engine.snapshot_into(&mut buf).unwrap();

    let row = extract_row(&buf, 0, None);

    assert_eq!(row.cells()[0].background, row.cells()[1].background);
}

#[test]
fn selection_overlay_uses_selection_background() {
    let mut engine = GhosttyTerminal::new(4, 1, 100).unwrap();
    engine.write_vt(b"abcd");
    let mut buf = RenderBuffer::new(4, 1);
    engine.snapshot_into(&mut buf).unwrap();
    let selection = SelectionRange::new(
        Pos::new(Line(0), Column(1)),
        Pos::new(Line(0), Column(2)),
        false,
    );

    let frame = TerminalFrame::from_render_buffer_with_selection(
        &buf,
        Some(selection),
        &GenerationMap::new(),
    );
    let selected = TerminalColor::from_color_arr(Colors::default().selection_background);
    let cells = frame.lines()[0].cells();

    assert_eq!(cells[0].background, None);
    assert_eq!(cells[1].background, Some(selected));
    assert_eq!(cells[2].background, Some(selected));
    assert_eq!(cells[3].background, None);
}

#[test]
fn wide_char_gets_placeholder_and_runs_cover_text() {
    let mut engine = GhosttyTerminal::new(6, 1, 100).unwrap();
    engine.write_vt("中A".as_bytes());
    let mut buf = RenderBuffer::new(6, 1);
    engine.snapshot_into(&mut buf).unwrap();
    let row = extract_row(&buf, 0, None);

    // The wide glyph is followed by a blank placeholder for its 2nd column.
    assert!(row.text().as_ref().starts_with("中\u{00a0}A"));
    // Force-width layout needs run byte-lengths to sum to the row text length.
    let run_bytes: usize = row.runs().iter().map(|run| run.len).sum();
    assert_eq!(run_bytes, row.text().len());
}

#[test]
fn inverse_swaps_foreground_into_the_painted_background() {
    // Inverse video paints the cell background with what would be the
    // foreground color, so a plain 'A' fg equals the inverse 'A' background.
    let mut plain_engine = GhosttyTerminal::new(4, 1, 100).unwrap();
    plain_engine.write_vt(b"A");
    let mut plain_buf = RenderBuffer::new(4, 1);
    plain_engine.snapshot_into(&mut plain_buf).unwrap();
    let plain = extract_row(&plain_buf, 0, None);

    let mut engine = GhosttyTerminal::new(4, 1, 100).unwrap();
    engine.write_vt(b"\x1b[7mA");
    let mut buf = RenderBuffer::new(4, 1);
    engine.snapshot_into(&mut buf).unwrap();
    let inverse = extract_row(&buf, 0, None);

    assert_eq!(inverse.cells()[0].background, Some(plain.runs()[0].fg));
}

#[test]
fn text_styles_become_distinct_style_runs() {
    let mut engine = GhosttyTerminal::new(8, 1, 100).unwrap();
    // Bold B, italic I, underline U, strikethrough S, each reset between.
    engine.write_vt(b"\x1b[1mB\x1b[0m\x1b[3mI\x1b[0m\x1b[4mU\x1b[0m\x1b[9mS\x1b[0m");
    let mut buf = RenderBuffer::new(8, 1);
    engine.snapshot_into(&mut buf).unwrap();
    let row = extract_row(&buf, 0, None);

    assert!(
        row.runs()
            .iter()
            .any(|r| r.bold && !r.italic && !r.underline && !r.strikethrough)
    );
    assert!(row.runs().iter().any(|r| r.italic && !r.bold));
    assert!(row.runs().iter().any(|r| r.underline && !r.strikethrough));
    assert!(row.runs().iter().any(|r| r.strikethrough && !r.underline));
}

#[test]
fn bold_toggle_changes_shape_cache_key() {
    let mut plain_engine = GhosttyTerminal::new(4, 1, 100).unwrap();
    plain_engine.write_vt(b"A");
    let mut plain_buf = RenderBuffer::new(4, 1);
    plain_engine.snapshot_into(&mut plain_buf).unwrap();
    let plain = extract_row(&plain_buf, 0, None);

    let mut bold_engine = GhosttyTerminal::new(4, 1, 100).unwrap();
    bold_engine.write_vt(b"\x1b[1mA");
    let mut bold_buf = RenderBuffer::new(4, 1);
    bold_engine.snapshot_into(&mut bold_buf).unwrap();
    let bold = extract_row(&bold_buf, 0, None);

    // Same visible text, but bold must not reuse the plain shaped glyphs.
    assert_eq!(plain.text(), bold.text());
    assert_ne!(plain.text_hash(), bold.text_hash());
}

#[test]
fn extracts_cursor_shape_without_mutating_row_text() {
    let mut engine = GhosttyTerminal::new(4, 1, 100).unwrap();
    engine.write_vt(b"\x1b[5 qA\x1b[D");
    let mut buf = RenderBuffer::new(4, 1);
    engine.snapshot_into(&mut buf).unwrap();

    let frame = TerminalFrame::from_render_buffer(&buf);
    let row = &frame.lines()[0];

    assert_eq!(frame.cursor().unwrap().shape, CursorShape::Beam);
    assert!(row.text().as_ref().starts_with("A\u{00a0}"));
    assert_eq!(row.runs()[0].fg, theme_default_foreground());
}

// --- Kitty image frame extraction ---

use crate::terminal::graphics::graphic_to_generation;

/// Run `vt` through the engine, mirror it into a `RenderBuffer`, and build a live
/// generation map from the shipped image deltas — the same inputs frame extraction
/// sees at runtime.
fn buf_and_generations(cols: u16, rows: u16, vt: &[u8]) -> (RenderBuffer, GenerationMap) {
    let mut engine = GhosttyTerminal::new(cols, rows, 100).unwrap();
    engine.resize(cols, rows, 10, 20).unwrap();
    engine.write_vt(vt);
    let buf = engine.snapshot().unwrap();

    let release: terminal::graphics::ReleaseQueue = Default::default();
    let (pending, _) = engine.take_image_deltas(buf.placements());
    let mut generations = GenerationMap::new();
    for (id, data) in pending {
        if let Some(g) = graphic_to_generation(data, &release) {
            generations.insert(id, g);
        }
    }
    (buf, generations)
}

#[test]
fn extracts_ordinary_placement_with_source_and_z() {
    let (buf, generations) =
        buf_and_generations(20, 5, b"\x1b_Ga=T,f=32,s=1,v=1,i=1,p=9;/wAA/w==\x1b\\");
    let images = extract_frame_images(&buf, &generations);
    assert_eq!(images.len(), 1, "one ordinary image");
    let img = &images[0];
    assert_eq!(img.z_layer(), ZLayer::AboveText, "z=0 paints above text");
    match img.kind {
        FrameImageKind::Ordinary {
            viewport_col,
            viewport_row,
            source,
            ..
        } => {
            assert_eq!((viewport_col, viewport_row), (0, 0));
            assert_eq!(source, [0.0, 0.0, 1.0, 1.0], "full-image source");
        }
        _ => panic!("expected ordinary"),
    }
}

#[test]
fn ordinary_destination_maps_cells_to_pixels() {
    let (buf, generations) =
        buf_and_generations(20, 5, b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/wAA/w==\x1b\\");
    let img = &extract_frame_images(&buf, &generations)[0];
    // cell 10x20, viewport (0,0), no offsets: dest = one cell, full source.
    let (dest, source) = img.destination(10.0, 20.0, 100.0, 50.0, 0.0).unwrap();
    assert_eq!(dest, [100.0, 50.0, 10.0, 20.0]);
    assert_eq!(source, [0.0, 0.0, 1.0, 1.0]);
    // A row displacement (fixed-bottom / block-list) shifts y only.
    let (dest2, _) = img.destination(10.0, 20.0, 100.0, 50.0, 7.0).unwrap();
    assert_eq!(dest2[1], 57.0);
}

#[test]
fn destination_maps_negative_viewport_row_above_origin() {
    // A placement scrolled one row above the viewport top → negative dest y (paint
    // clips it to the content mask).
    let (buf, generations) =
        buf_and_generations(20, 5, b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/wAA/w==\x1b\\");
    let mut img = extract_frame_images(&buf, &generations).remove(0);
    if let FrameImageKind::Ordinary { viewport_row, .. } = &mut img.kind {
        *viewport_row = -1;
    }
    let (dest, _) = img.destination(10.0, 20.0, 0.0, 0.0, 0.0).unwrap();
    assert_eq!(dest[1], -20.0, "one row above the origin");
}

#[test]
fn skips_placement_whose_image_is_not_cached() {
    // Same buffer, but an empty generation map (pixels not yet delivered).
    let (buf, _) = buf_and_generations(20, 5, b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/wAA/w==\x1b\\");
    let images = extract_frame_images(&buf, &GenerationMap::new());
    assert!(images.is_empty(), "uncached image is skipped, not failed");
}

#[test]
fn plain_rows_are_not_scanned_for_placeholders() {
    // No virtual placeholders anywhere: extraction yields no virtual images and the
    // per-row fast path skips every row (no panic, empty result).
    let (buf, generations) = buf_and_generations(8, 2, b"hello");
    assert!(!buf.row_has_virtual_placeholder(0));
    assert!(extract_frame_images(&buf, &generations).is_empty());
}

#[test]
fn extracts_contiguous_virtual_run() {
    // A 2×1 virtual image (id=7, p=3, c=2 r=1) with two contiguous placeholder
    // cells that inherit column from the first → one run of width 2.
    // Placement id 0 (no `p=`, no underline color) so the run's decoded
    // placement id (from underline) matches the placement metadata.
    let d0 = DIACRITICS[0];
    let cell0 = format!("\x1b[38;2;0;0;7m{}{}", '\u{10EEEE}', d0); // row=0,col=0
    let cell1 = format!("{}", '\u{10EEEE}'); // inherit row/col
    let mut vt = Vec::new();
    vt.extend_from_slice(b"\x1b_Ga=T,U=1,f=32,s=2,v=1,i=7,c=2,r=1;/wAA//8AAP8=\x1b\\");
    vt.extend_from_slice(cell0.as_bytes());
    vt.extend_from_slice(cell1.as_bytes());
    let (buf, generations) = buf_and_generations(20, 5, &vt);

    let images = extract_frame_images(&buf, &generations);
    assert_eq!(images.len(), 1, "one virtual run");
    match images[0].kind {
        FrameImageKind::Virtual {
            run,
            placement_cols,
            screen_col,
            screen_line,
            ..
        } => {
            assert_eq!(run.image_id, 7);
            assert_eq!(run.width, 2, "two inherited-column cells form one run");
            assert_eq!(placement_cols, 2);
            assert_eq!((screen_line, screen_col), (0, 0));
        }
        _ => panic!("expected virtual"),
    }
}

#[test]
fn unmatched_placeholder_is_skipped() {
    // Placeholder cells reference image id 9, but no image 9 was transmitted, so
    // there is no matching virtual placement and no cached image → skipped.
    let d0 = DIACRITICS[0];
    let cell = format!("\x1b[38;2;0;0;9m{}{}{}", '\u{10EEEE}', d0, d0);
    let (buf, generations) = buf_and_generations(20, 5, cell.as_bytes());
    assert!(
        extract_frame_images(&buf, &generations).is_empty(),
        "no matching placement/image → no descriptor, no marker"
    );
}

#[test]
fn placeholder_codepoint_is_suppressed_from_text() {
    let d0 = DIACRITICS[0];
    let mut vt = Vec::new();
    vt.extend_from_slice(b"\x1b_Ga=T,U=1,f=32,s=1,v=1,i=7,p=3,c=1,r=1;/wAA/w==\x1b\\");
    vt.extend_from_slice(format!("\x1b[38;2;0;0;7m{}{}{}", '\u{10EEEE}', d0, d0).as_bytes());
    let (buf, generations) = buf_and_generations(20, 5, &vt);
    let frame = TerminalFrame::from_render_buffer_with_selection(&buf, None, &generations);
    // The placeholder glyph never reaches shaped text (no U+10EEEE), but the cell
    // still occupies its column (blank).
    assert!(
        !frame.lines()[0].text().as_ref().contains('\u{10EEEE}'),
        "placeholder codepoint suppressed"
    );
}

#[test]
fn z_layer_buckets_by_protocol_thresholds() {
    // Pure classifier check across the three protocol layers.
    let (buf, generations) =
        buf_and_generations(20, 5, b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/wAA/w==\x1b\\");
    let mut img = extract_frame_images(&buf, &generations).remove(0);
    img.z = i32::MIN;
    assert_eq!(img.z_layer(), ZLayer::BelowBackground);
    img.z = -1;
    assert_eq!(img.z_layer(), ZLayer::BelowText);
    img.z = 0;
    assert_eq!(img.z_layer(), ZLayer::AboveText);
}
