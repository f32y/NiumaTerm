use std::{collections, io, thread};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use image_rs::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use nmt_config::colors::AnsiColor;

use crate::ghostty::*;

fn line_text(snapshot: &RenderBuffer, row: usize) -> String {
    let mut text = String::new();
    for x in 0..snapshot.cols() {
        let cell = snapshot.cell(x, row);
        if cell.c() == '\0'
            || matches!(
                cell.wide(),
                terminal::square::Wide::Spacer | terminal::square::Wide::LeadingSpacer
            )
        {
            continue;
        }
        text.push(cell.c());
        if let Some(extras) = cell.extras_id().and_then(|id| snapshot.extras().get(&id)) {
            text.extend(&extras.zerowidth);
        }
    }
    text
}

/// Finishing freezes content into an engine block readable
/// through the block row visitor; the active screen restarts empty with
/// SGR carried over; stale handles read as absent.
#[test]
fn finish_block_freezes_and_reads_back() {
    let mut t = GhosttyTerminal::new(20, 5, 10_000).unwrap();

    // Empty screen: no block.
    assert!(t.finish_block().unwrap().is_none());

    // Bold "hello" + newline + "world".
    t.write_vt(b"\x1b[1mhello\r\nworld");
    let handle = t.finish_block().unwrap().expect("block created");
    assert_eq!(t.block_count(), 1);
    assert_eq!(t.block_at(0).map(|h| h.id), Some(handle.id));
    assert_eq!(t.block_row_count(handle), Some(2));
    assert_eq!(t.block_cols(handle), Some(20));

    let row0 = t.read_block_row(handle, 0).unwrap().expect("row 0");
    let text: String = row0.cells.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(text, "hello");
    assert!(row0.cells[0].style.bold, "SGR captured in frozen block");
    let row1 = t.read_block_row(handle, 1).unwrap().expect("row 1");
    let text: String = row1.cells.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(text, "world");
    assert!(
        t.read_block_row(handle, 2).unwrap().is_none(),
        "beyond logical rows"
    );

    // Active screen restarted empty; SGR continues (bold pen).
    let snap = t.snapshot().unwrap();
    assert_eq!((snap.cursor().col.0, snap.cursor().row.0), (0, 0));
    t.write_vt(b"next");
    let row = t.read_screen_row(0).unwrap().expect("active row");
    assert!(row.cells[0].style.bold, "continuation SGR applies");

    // The frozen block never changes.
    let row0 = t.read_block_row(handle, 0).unwrap().expect("row 0 again");
    let text: String = row0.cells.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(text, "hello");

    // Stale after removal; ids are never reused.
    assert!(t.remove_block(handle));
    assert!(!t.remove_block(handle));
    assert_eq!(t.block_row_count(handle), None);
    assert_eq!(t.block_count(), 0);

    t.write_vt(b"again");
    let h2 = t.finish_block().unwrap().expect("second block");
    assert_ne!(h2.id, handle.id);
    t.clear_blocks();
    assert_eq!(t.block_count(), 0);
}

/// A stream RIS (`ESC c`) clears only the active screen — finished
/// blocks survive so a per-sample reset does not erase frozen history.
#[test]
fn finish_block_survives_stream_ris() {
    let mut t = GhosttyTerminal::new(20, 5, 10_000).unwrap();
    t.write_vt(b"hello");
    let handle = t.finish_block().unwrap().expect("block created");
    t.write_vt(b"\x1bc");
    assert_eq!(t.block_count(), 1);
    assert_eq!(t.block_row_count(handle), Some(1));
}

/// An acquired block reference reads rows and text without the
/// terminal, survives removal (deferred destroy), and format-exports.
#[test]
fn block_ref_reads_and_survives_removal() {
    let mut t = GhosttyTerminal::new(20, 5, 10_000).unwrap();
    t.write_vt(b"\x1b[1mhello\r\nworld");
    let handle = t.finish_block().unwrap().expect("block created");

    let r = t.block_acquire(handle).expect("acquire");
    assert_eq!(r.handle().id, handle.id);
    assert_eq!(r.row_count(), 2);
    assert_eq!(r.cols(), 20);
    assert!(r.bytes() > 0);

    let palette = t.color_palette();
    let mut text = String::new();
    let meta = r
        .read_row_visit(0, &palette, |_, cell_text, _, style| {
            text.push_str(cell_text.as_str());
            assert!(style.bold);
        })
        .unwrap()
        .expect("row 0");
    assert!(!meta.wrapped);
    assert_eq!(text, "hello");
    assert!(
        r.read_row_visit(2, &palette, |_, _, _, _| {})
            .unwrap()
            .is_none()
    );

    assert_eq!(
        r.format_range((0, 0), (1, 19), true, true)
            .unwrap()
            .trim_end(),
        "hello\nworld"
    );

    // Remove while held: handle stale, snapshot still readable.
    assert!(t.remove_block(handle));
    assert!(t.block_acquire(handle).is_none());
    assert_eq!(r.row_count(), 2);
    drop(r);
}

/// Block references can be read and released on another thread
/// while the writer keeps finishing, resizing (reflow drains readers),
/// and removing blocks.
#[test]
fn block_ref_cross_thread_reads() {
    let mut t = GhosttyTerminal::new(20, 5, 10_000).unwrap();
    let palette = t.color_palette();

    let (tx, rx) = sync::mpsc::channel::<BlockRef>();
    let reader = thread::spawn(move || {
        let mut cells = 0usize;
        for r in rx {
            let rows = r.row_count();
            for row in 0..rows {
                let _ = r.read_row_visit(row, &palette, |_, _, _, _| cells += 1);
            }
            if rows > 0 {
                let _ = r.format_range((0, 0), (rows - 1, r.cols() - 1), true, true);
            }
        }
        cells
    });

    let mut cols = 20u16;
    for i in 0..60 {
        t.write_vt(b"the quick brown fox jumps over the lazy dog\r\n");
        let handle = t.finish_block().unwrap().expect("block created");
        if let Some(r) = t.block_acquire(handle) {
            tx.send(r).unwrap();
        }
        if i % 10 == 9 {
            // Reflow of every block: the engine drains reader refs
            // (including any still queued in the channel) per block.
            cols = if cols == 20 { 26 } else { 20 };
            t.resize(cols, 5, 10, 20).unwrap();
        }
        if i % 15 == 14 {
            // Deferred destroy while the reader may hold the ref.
            t.remove_block(handle);
        }
    }
    drop(tx);
    let cells = reader.join().unwrap();
    assert!(cells > 0, "reader observed content");
}

/// Shrinking the block budget evicts oldest-first immediately,
/// keeping the newest; blocks_bytes reports the enforced total.
#[test]
fn block_budget_evicts_oldest() {
    let mut t = GhosttyTerminal::new(20, 5, 10_000).unwrap();
    let mut last = None;
    for _ in 0..3 {
        t.write_vt(b"hello");
        last = t.finish_block().unwrap();
    }
    assert_eq!(t.block_count(), 3);
    assert!(t.blocks_bytes() > 0);

    t.set_block_budget_bytes(1).unwrap();
    assert_eq!(t.block_count(), 1);
    assert_eq!(t.block_at(0).map(|h| h.id), last.map(|h| h.id));
}

/// Resize rewraps finished blocks to the new width and
/// bumps their data generation; block reads follow the new layout.
#[test]
fn block_reflows_on_resize() {
    let mut t = GhosttyTerminal::new(10, 5, 10_000).unwrap();
    t.write_vt(b"0123456789ABC"); // wraps into 2 rows at 10 cols
    let handle = t.finish_block().unwrap().expect("block created");
    assert_eq!(t.block_row_count(handle), Some(2));

    t.resize(5, 5, 10, 20).unwrap();
    assert_eq!(t.block_row_count(handle), Some(3));
    assert_eq!(t.block_cols(handle), Some(5));
    let generation = t.block_at(0).map(|h| h.generation);
    assert_eq!(generation, Some(handle.generation + 1));

    let row = t.read_block_row(handle, 1).unwrap().expect("row 1");
    let text: String = row.cells.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(text, "56789");
}

/// Transmitting and placing a Kitty image surfaces a non-virtual placement
/// in the snapshot at the cursor's viewport position.
#[test]
fn kitty_image_placement() {
    let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
    // Give the engine a cell geometry so placement pixel/grid size resolves.
    t.resize(20, 5, 10, 20).unwrap();

    // Transmit + display (a=T) a 1×1 RGBA image (f=32), id=1, placement p=9.
    // Payload is one opaque-red pixel (FF 00 00 FF) base64-encoded.
    t.write_vt(b"\x1b_Ga=T,f=32,s=1,v=1,i=1,p=9;/wAA/w==\x1b\\");

    let snap = t.snapshot().unwrap();
    let visible: Vec<_> = snap.placements().iter().filter(|p| !p.is_virtual).collect();
    assert_eq!(visible.len(), 1, "one non-virtual placement");
    let p = visible[0];
    assert_eq!(p.image_id, 1);
    assert_eq!(
        p.placement_id, 9,
        "ordinary placement carries its placement id"
    );
    assert_eq!((p.viewport_col, p.viewport_row), (0, 0), "placed at cursor");
    assert!(p.grid_cols >= 1 && p.grid_rows >= 1, "spans >=1 cell");
    assert!(
        p.pixel_width >= 1 && p.pixel_height >= 1,
        "has rendered pixels"
    );
    // Ordinary geometry unchanged: full 1×1 source rectangle, no sub-cell offset.
    assert_eq!(
        (p.source_x, p.source_y, p.source_width, p.source_height),
        (0, 0, 1, 1),
        "full-image source rectangle"
    );
    assert_eq!(
        (p.cell_x_offset, p.cell_y_offset),
        (0, 0),
        "no sub-cell offset"
    );

    // The delta reader ships each pixel generation exactly once.
    let (first, removed) = t.take_image_deltas(snap.placements());
    assert!(removed.is_empty(), "nothing removed on first ship");
    assert!(
        first.iter().any(|(id, _)| *id == 1),
        "first batch ships image 1's pixels"
    );
    // A second call with no intervening write must yield nothing — neither a
    // re-ship nor a removal (idempotent steady state).
    let snap2 = t.snapshot().unwrap();
    let (second, removed2) = t.take_image_deltas(snap2.placements());
    assert!(
        second.is_empty() && removed2.is_empty(),
        "unchanged batch: {} pending / {} removed (want 0/0)",
        second.len(),
        removed2.len()
    );
}

/// The backend delta key is `(id, width, height,
/// data_len)` because the pinned FFI exposes no image generation counter. A
/// same-ID retransmission whose width, height, and byte length are unchanged
/// is therefore NOT observed as a delta and is not re-shipped, even if the
/// pixel bytes differ. This known limitation needs a future Ghostty generation
/// field to distinguish same-sized retransmissions.
#[test]
fn kitty_same_size_retransmit_not_reshipped() {
    let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
    t.resize(20, 5, 10, 20).unwrap();

    // Transmit + place a 1×1 opaque-red RGBA image, id=1.
    t.write_vt(b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/wAA/w==\x1b\\");
    let snap = t.snapshot().unwrap();
    let (first, _) = t.take_image_deltas(snap.placements());
    assert!(first.iter().any(|(id, _)| *id == 1), "first ship");

    // Retransmit the SAME id with the SAME 1×1 RGBA dimensions/length but
    // different pixels (opaque-blue). Same (id,w,h,len) key ⇒ not re-shipped.
    t.write_vt(b"\x1b_Ga=T,f=32,s=1,v=1,i=1;AAD/fw==\x1b\\");
    let snap = t.snapshot().unwrap();
    let (second, removed) = t.take_image_deltas(snap.placements());
    assert!(
        second.iter().all(|(id, _)| *id != 1) && !removed.contains(&1),
        "same-size same-id retransmission is not re-shipped (known residual)"
    );
}

/// With the registered PNG decode hook, an `f=100` transmission is
/// decoded by the engine to RGBA and shipped by `take_image_deltas`.
#[test]
fn kitty_png_decode() {
    use base64::Engine as _;

    let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
    t.resize(20, 5, 10, 20).unwrap();

    // A 1×1 opaque-red PNG, generated so the bytes are unquestionably valid.
    let img = RgbaImage::from_pixel(1, 1, Rgba([255, 0, 0, 255]));
    let mut png = Vec::new();
    DynamicImage::ImageRgba8(img)
        .write_to(&mut io::Cursor::new(&mut png), ImageFormat::Png)
        .unwrap();
    let b64 = STANDARD.encode(&png);

    t.write_vt(format!("\x1b_Ga=T,f=100,i=2;{b64}\x1b\\").as_bytes());

    let snap = t.snapshot().unwrap();
    let (pending, _) = t.take_image_deltas(snap.placements());
    let img = pending
        .iter()
        .find(|(id, _)| *id == 2)
        .map(|(_, d)| d)
        .expect("PNG image decoded + shipped");
    assert_eq!((img.width, img.height), (1, 1));
    assert_eq!(img.color_type, graphics::ColorType::Rgba);
    assert_eq!(img.pixels.len(), 4, "1×1 RGBA = 4 bytes");
}

/// A Kitty Unicode-placeholder cell (U+10EEEE, image id in the foreground)
/// sets the per-row `KITTY_VIRTUAL_PLACEHOLDER` flag, the snapshot reports a
/// virtual placement carrying the id, and its pixels still ship via the
/// delta path (virtual placements).
#[test]
fn virtual_placeholder_row_flag() {
    let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
    t.resize(20, 5, 10, 20).unwrap();

    // Transmit a 1×2 RGBA image (id=7) as a *virtual* placement (U=1) with an
    // explicit placement id (p=3), grid size (c=2,r=1), and z (z=5): it has no
    // engine grid position; placeholders position it, but its identity, grid
    // size, and z must be exposed so the frame path can match runs.
    t.write_vt(b"\x1b_Ga=T,U=1,f=32,s=1,v=2,i=7,p=3,c=2,r=1,z=5;/wAA//8AAP8=\x1b\\");
    // Print one placeholder cell on row 0 with the image id (7) in the fg.
    let cell = format!("\x1b[38;2;0;0;7m{}", '\u{10EEEE}');
    t.write_vt(cell.as_bytes());

    let snap = t.snapshot().unwrap();
    assert!(
        snap.row_has_virtual_placeholder(0),
        "row 0 carries the virtual-placeholder flag"
    );
    assert!(
        (1..snap.rows()).all(|y| !snap.row_has_virtual_placeholder(y)),
        "no other row is flagged"
    );

    let virt: Vec<_> = snap.placements().iter().filter(|p| p.is_virtual).collect();
    assert_eq!(virt.len(), 1, "one virtual placement");
    let v = virt[0];
    assert_eq!(v.image_id, 7);
    assert_eq!(v.placement_id, 3, "virtual placement id exposed");
    assert_eq!(
        (v.grid_cols, v.grid_rows),
        (2, 1),
        "virtual grid size exposed"
    );
    assert_eq!(v.z, 5, "virtual z-index exposed");

    // Virtual placements ship pixels through the same delta path.
    let (pending, _) = t.take_image_deltas(snap.placements());
    assert!(
        pending.iter().any(|(id, _)| *id == 7),
        "virtual placement's image pixels ship by id"
    );
}

/// Scrolling moves a placement's viewport row by the scroll delta. An image
/// scrolled fully off-screen is not reported removed by `take_image_deltas`
/// because it remains live in the engine; scrolling must not emit graphics churn.
#[test]
fn kitty_image_scroll() {
    let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
    t.resize(20, 5, 10, 20).unwrap();

    // Lay down `a b c <image> d e f g h` so the image lands at absolute row 3
    // and is pushed into scrollback (9 rows, 5-row viewport).
    t.write_vt(b"a\r\nb\r\nc\r\n");
    t.write_vt(b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/wAA/w==\x1b\\");
    t.write_vt(b"\r\nd\r\ne\r\nf\r\ng\r\nh");

    let find_row = |t: &mut GhosttyTerminal| -> Option<i32> {
        let snap = t.snapshot().unwrap();
        snap.placements()
            .iter()
            .find(|p| p.image_id == 1 && !p.is_virtual)
            .map(|p| p.viewport_row)
    };

    // Scroll up so the image is visible; record its viewport row, ship it.
    t.scroll_viewport_bottom();
    t.scroll_viewport_delta(-2);
    let r0 = find_row(&mut t).expect("image visible after scrolling up 2");
    let snap = t.snapshot().unwrap();
    let (shipped, _) = t.take_image_deltas(snap.placements());
    assert!(
        shipped.iter().any(|(id, _)| *id == 1),
        "shipped while visible"
    );

    // One more row up moves a fixed placement down by exactly one.
    t.scroll_viewport_delta(-1);
    let r1 = find_row(&mut t).expect("image still visible after one more row");
    assert_eq!((r1 - r0).abs(), 1, "viewport row moves by the scroll delta");

    // Scroll back to the bottom so the image is fully off-screen, then run a
    // delta pass: the image is still in the engine, so it must NOT be removed
    // (and not re-shipped).
    t.scroll_viewport_bottom();
    let snap = t.snapshot().unwrap();
    assert!(
        !snap
            .placements()
            .iter()
            .any(|p| p.image_id == 1 && !p.is_virtual),
        "off-screen: no visible placement"
    );
    let (pending, removed) = t.take_image_deltas(snap.placements());
    assert!(
        !removed.contains(&1),
        "off-screen image must not be removed"
    );
    assert!(
        !pending.iter().any(|(id, _)| *id == 1),
        "off-screen image must not be re-shipped"
    );
}

/// Deleting an image with `d=I` frees its data, removes its placement from
/// the snapshot and reports the id in `take_image_deltas`' remove queue.
#[test]
fn kitty_image_delete() {
    let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
    t.resize(20, 5, 10, 20).unwrap();

    t.write_vt(b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/wAA/w==\x1b\\");
    let snap = t.snapshot().unwrap();
    let (shipped, _) = t.take_image_deltas(snap.placements());
    assert!(
        shipped.iter().any(|(id, _)| *id == 1),
        "shipped before delete"
    );

    // Delete image id=1 and free its data (uppercase d=I).
    t.write_vt(b"\x1b_Ga=d,d=I,i=1\x1b\\");
    let snap = t.snapshot().unwrap();
    assert!(
        !snap.placements().iter().any(|p| p.image_id == 1),
        "no placement remains after delete"
    );
    let (_, removed) = t.take_image_deltas(snap.placements());
    assert!(removed.contains(&1), "deleted image reported for removal");
}

/// DECSCUSR shape and DECTCEM visibility land in the snapshot cursor.
#[test]
fn snapshot_captures_cursor_style() {
    use crate::ansi::CursorShape;
    let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();

    t.write_vt(b"\x1b[2 q"); // steady block
    assert_eq!(t.snapshot().unwrap().cursor_shape(), CursorShape::Block);
    t.write_vt(b"\x1b[5 q"); // steady bar
    assert_eq!(t.snapshot().unwrap().cursor_shape(), CursorShape::Beam);
    t.write_vt(b"\x1b[3 q"); // blinking underline
    assert_eq!(t.snapshot().unwrap().cursor_shape(), CursorShape::Underline);

    assert!(t.snapshot().unwrap().cursor_visible(), "visible by default");
    t.write_vt(b"\x1b[?25l"); // DECTCEM hide
    assert!(
        !t.snapshot().unwrap().cursor_visible(),
        "hidden after DECTCEM"
    );
    t.write_vt(b"\x1b[?25h");
    assert!(t.snapshot().unwrap().cursor_visible(), "shown again");
}

#[test]
fn configured_cursor_shape_is_the_decscusr_default() {
    use crate::ansi::CursorShape;
    let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();

    t.set_default_cursor_shape(CursorShape::Beam).unwrap();
    assert_eq!(t.snapshot().unwrap().cursor_shape(), CursorShape::Beam);

    t.write_vt(b"\x1b[2 q");
    t.set_default_cursor_shape(CursorShape::Underline).unwrap();
    assert_eq!(t.snapshot().unwrap().cursor_shape(), CursorShape::Block);

    t.write_vt(b"\x1b[0 q");
    assert_eq!(t.snapshot().unwrap().cursor_shape(), CursorShape::Underline);
}

/// OSC 10/11 dynamic foreground and background land in the snapshot colors.
/// The 256-entry palette enters through `set_colors`; snapshots capture only
/// foreground, background, and the background override.
#[test]
fn snapshot_captures_colors() {
    use nmt_config::colors::{ColorRgb, NamedColor};
    let mut t = GhosttyTerminal::new(8, 3, 100).unwrap();
    t.set_colors(
        [205, 214, 244],
        [15, 13, 14],
        [180, 190, 254],
        &[[0u8; 3]; 256],
    );

    t.write_vt(b"\x1b]10;#112233\x07"); // OSC 10 set foreground
    assert_eq!(
        t.snapshot().unwrap().colors()[NamedColor::Foreground],
        Some(
            ColorRgb {
                r: 0x11,
                g: 0x22,
                b: 0x33
            }
            .to_arr()
        ),
        "OSC 10 sets the effective foreground"
    );

    t.write_vt(b"\x1b]11;#445566\x07"); // OSC 11 set background
    assert_eq!(
        t.snapshot().unwrap().window_bg_override(),
        Some(ColorRgb {
            r: 0x44,
            g: 0x55,
            b: 0x66
        }),
        "OSC 11 sets the background override"
    );
}

#[test]
fn theme_colors_update_engine_defaults() {
    use nmt_config::colors::{ColorRgb, Colors, NamedColor};

    let mut terminal = GhosttyTerminal::new(8, 3, 100).unwrap();
    let colors = Colors::default();
    terminal.set_theme_colors(&colors);

    let snapshot = terminal.snapshot().unwrap();
    assert_eq!(
        snapshot.colors()[NamedColor::Foreground],
        Some(ColorRgb::from_color_arr(colors.foreground).to_arr())
    );
    assert_eq!(
        snapshot.colors()[NamedColor::Background],
        Some(ColorRgb::from_color_arr(colors.background.0).to_arr())
    );
}

/// A VT mode set/reset round-trips through the engine `mode()` reader
/// and feeds the lock-free per-panel atomic consumed by the input path.
#[test]
fn vt_mode_get_roundtrip() {
    let mut t = GhosttyTerminal::new(8, 3, 100).unwrap();

    assert!(!t.mode(mode::CURSOR_KEYS), "app-cursor off by default");
    t.write_vt(b"\x1b[?1h"); // DECCKM on
    assert!(t.mode(mode::CURSOR_KEYS), "DECCKM on after ?1h");
    t.write_vt(b"\x1b[?1l"); // DECCKM off
    assert!(!t.mode(mode::CURSOR_KEYS), "DECCKM off after ?1l");

    // Alt screen toggles independently.
    t.write_vt(b"\x1b[?1049h");
    assert!(t.mode(mode::ALT_SCREEN), "alt-screen on");
    t.write_vt(b"\x1b[?1049l");
    assert!(!t.mode(mode::ALT_SCREEN), "alt-screen off");
}

/// A small storage limit evicts older images once exceeded; only the
/// retained image is still in the engine store.
#[test]
fn kitty_storage_limit() {
    let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
    t.resize(20, 5, 10, 20).unwrap();
    // Room for ~one small image: a 2×2 RGBA is 16 bytes.
    t.set_kitty_storage_limit(24);

    // Two distinct 2×2 RGBA images (ids 1, 2). Base64 of 16 bytes = 24 chars.
    let px = STANDARD.encode([0u8; 16]);
    t.write_vt(format!("\x1b_Ga=t,f=32,s=2,v=2,i=1;{px}\x1b\\").as_bytes());
    t.write_vt(format!("\x1b_Ga=t,f=32,s=2,v=2,i=2;{px}\x1b\\").as_bytes());

    // The newest image survives; the oldest was evicted to honour the limit.
    assert!(t.kitty_image_exists(2), "newest image retained");
    assert!(
        !t.kitty_image_exists(1),
        "oldest image evicted by the limit"
    );
}

/// a sixel sequence is ignored (terminal drops sixel) without panicking and
/// leaves a valid, image-free snapshot.
#[test]
fn sixel_ignored_no_crash() {
    let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
    t.resize(20, 5, 10, 20).unwrap();
    t.write_vt(b"\x1bPq#0;2;100;0;0#0~~~~~\x1b\\");
    let snap = t.snapshot().unwrap();
    assert!(
        snap.placements().is_empty(),
        "no kitty placements from sixel"
    );
}

/// an iTerm2 inline-image (OSC 1337) is ignored without panicking and
/// leaves a valid, image-free snapshot.
#[test]
fn iterm2_ignored_no_crash() {
    let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
    t.resize(20, 5, 10, 20).unwrap();
    t.write_vt(b"\x1b]1337;File=inline=1:AAAA\x07");
    let snap = t.snapshot().unwrap();
    assert!(
        snap.placements().is_empty(),
        "no kitty placements from iTerm2"
    );
}

/// OSC 133 shell-integration marks are an unknown OSC to the engine and must
/// be ignored. The PTY sniffer forwards those marks unchanged, so they must
/// leave only the visible text, no garbage cells.
#[test]
fn osc133_marks_ignored_no_crash() {
    let mut t = GhosttyTerminal::new(20, 5, 100).unwrap();
    t.resize(20, 5, 10, 20).unwrap();
    // ESC]133;A BEL  P>  ESC]133;B BEL  ESC]133;C BEL  hi
    t.write_vt(b"\x1b]133;A\x07P>\x1b]133;B\x07\x1b]133;C\x07hi");
    let snap = t.snapshot().unwrap();
    assert_eq!(line_text(&snap, 0).trim_end(), "P>hi");
}

/// OSC 11 sets the window background as an override; OSC 111 resets it.
/// Exercises the exact FFI path the renderer reads (`snapshot().colors`).
#[test]
fn osc_11_set_and_111_reset_background() {
    let palette = [[0u8, 0, 0]; 256];
    let default_bg = [15u8, 13, 14];

    let run = |reset_seq: &[u8]| {
        let mut t = GhosttyTerminal::new(8, 3, 100).unwrap();
        t.set_colors([205, 214, 244], default_bg, [180, 190, 254], &palette);

        assert_eq!(
            t.snapshot().unwrap().window_bg_override(),
            None,
            "no override before any OSC"
        );

        t.write_vt(b"\x1b]11;#330000\x07");
        assert_eq!(
            t.snapshot().unwrap().window_bg_override(),
            Some(ColorRgb { r: 51, g: 0, b: 0 }),
            "OSC 11 sets the override"
        );

        t.write_vt(reset_seq);
        assert_eq!(
            t.snapshot().unwrap().window_bg_override(),
            None,
            "OSC 111 ({reset_seq:?}) resets the override",
        );
    };

    run(b"\x1b]111\x07"); // BEL-terminated
    run(b"\x1b]111\x1b\\"); // ST-terminated
}

#[test]
fn extracts_basic_vt_snapshot() {
    let mut terminal = GhosttyTerminal::new(8, 3, 100).unwrap();
    terminal.write_vt(b"hi \x1b[31mred\x1b[0m");

    let snapshot = terminal.snapshot().unwrap();

    assert_eq!(snapshot.cols(), 8);
    assert_eq!(snapshot.rows(), 3);
    assert_eq!(snapshot.cell(0, 0).c(), 'h');
    assert_eq!(snapshot.cell(3, 0).c(), 'r');
}

/// Verifies the selection-anchoring assumption: a SCREEN
/// coordinate stays pinned to the same content as new output scrolls that
/// content into scrollback. If this holds, selection anchors need no
/// rotate-on-output.
#[test]
fn screen_coords_stable_across_output() {
    let mut t = GhosttyTerminal::new(20, 3, 1000).unwrap();
    t.write_vt(b"AAAA\r\nBBBB\r\nCCCC");

    // Anchor row "AAAA" (viewport row 0) to a SCREEN coordinate.
    let r = t.viewport_grid_ref(0, 0).unwrap();
    let (_, screen_y) = t
        .point_from_grid_ref(&r, VtPointTag::SCREEN)
        .unwrap()
        .expect("viewport cell has a screen coord");

    // Output scrolls AAAA/BBBB/CCCC into history; viewport now DDDD/EEEE/FFFF.
    t.write_vt(b"\r\nDDDD\r\nEEEE\r\nFFFF");
    assert_eq!(line_text(&t.snapshot().unwrap(), 2), "FFFF");

    // The SAME screen coordinate still resolves to "AAAA".
    let start = t.grid_ref_at(VtPointTag::SCREEN, 0, screen_y).unwrap();
    let end = t.grid_ref_at(VtPointTag::SCREEN, 3, screen_y).unwrap();
    let mut sel = vt_sized!(VtSelection);
    sel.start = start;
    sel.end = end;
    let text = t.format_text(Some(&sel), false, true).unwrap();
    assert_eq!(text.trim_end(), "AAAA", "screen coord drifted: {text:?}");
}

/// Verifies the cheap screen↔viewport mapping for selection rendering: the
/// SCREEN coord of viewport row y is `viewport_top + y` for a single
/// `viewport_top` (so one cheap viewport grid_ref gives the whole mapping —
/// no expensive scrollbar read). Holds at the bottom and when scrolled.
#[test]
fn viewport_top_maps_screen_to_visible() {
    let mut t = GhosttyTerminal::new(20, 3, 1000).unwrap();
    t.write_vt(b"l0\r\nl1\r\nl2\r\nl3\r\nl4\r\nl5");

    let screen_of = |t: &GhosttyTerminal, y: u16| -> u32 {
        let r = t.viewport_grid_ref(0, y).unwrap();
        t.point_from_grid_ref(&r, VtPointTag::SCREEN)
            .unwrap()
            .unwrap()
            .1
    };

    // At bottom: each viewport row's screen coord differs by exactly 1, i.e.
    // a single viewport_top + y mapping.
    let top = screen_of(&t, 0);
    assert_eq!(screen_of(&t, 1), top + 1);
    assert_eq!(screen_of(&t, 2), top + 2);

    // Scrolled up: the mapping still holds, viewport_top just decreased.
    t.scroll_viewport_delta(-2);
    let top2 = screen_of(&t, 0);
    assert!(top2 < top, "viewport_top decreased on scroll up");
    assert_eq!(screen_of(&t, 1), top2 + 1);
    assert_eq!(screen_of(&t, 2), top2 + 2);
}

#[test]
fn resize_drag_does_not_accumulate_scrollback() {
    // DIAG (remove-crosswords resize-reflow bug). Simulate the user repro at
    // the engine layer: write content, shrink to the minimum, write more, then
    // "drag" the window by oscillating dimensions many times WITHOUT writing
    // any new content. The decisive question: does repeated resize alone grow
    // the engine's scrollback (sb_total) or visible rows? If it does, the
    // engine reflow is accumulating and the bug is upstream (libghostty-vt).
    let mut t = GhosttyTerminal::new(80, 24, 1000).unwrap();
    // Two `ls` runs worth of output.
    for i in 0..40 {
        t.write_vt(format!("file_{i:02}\r\n").as_bytes());
    }
    // Shrink to the minimum.
    t.resize(2, 1, 10, 20).unwrap();
    // Third `ls`.
    for i in 0..40 {
        t.write_vt(format!("f{i}\r\n").as_bytes());
    }
    let baseline = t.snapshot().unwrap().scrollbar().total;
    eprintln!("[resize-diag] baseline sb_total={baseline}");

    // Drag: oscillate the geometry, no new writes.
    let mut trace = Vec::new();
    for step in 0..30 {
        let cols = 2 + (step % 20) as u16 * 4;
        let rows = 1 + (step % 10) as u16 * 3;
        t.resize(cols.max(2), rows.max(1), 10, 20).unwrap();
        let snap = t.snapshot().unwrap();
        trace.push((cols, rows, snap.rows(), snap.scrollbar().total));
    }
    for (cols, rows, srows, total) in &trace {
        eprintln!(
            "[resize-diag] req cols={cols} rows={rows} -> snap.rows={srows} sb_total={total}"
        );
    }
    let final_total = trace.last().unwrap().3;
    // No new content was written during the drag, so total must not grow.
    // Reflow can legitimately re-wrap (total varies a little with width), but
    // it must not MONOTONICALLY accumulate. Flag gross growth.
    assert!(
        final_total <= baseline + 5,
        "engine scrollback grew under pure resize: baseline={baseline} final={final_total}"
    );
}

#[test]
fn resize_reflow_does_not_duplicate_viewport_content() {
    // DIAG (remove-crosswords resize-reflow DUP). The bug: after resize drags
    // the viewport shows the SAME content twice. Write uniquely-tagged long
    // lines (wide `ls`-like rows that wrap when the window narrows), push some
    // into scrollback, then oscillate the geometry. Every visible tag (ROWnnn /
    // DIRnnn) must appear AT MOST once in the viewport — twice means the engine
    // reflow duplicated content into the visible region.
    let mut t = GhosttyTerminal::new(120, 40, 2000).unwrap();
    t.resize(120, 40, 10, 20).unwrap();
    for i in 0..60 {
        // ~106 cols — wraps at narrow widths.
        t.write_vt(format!("ROW{i:03} {}\r\n", "x".repeat(100)).as_bytes());
    }
    t.resize(40, 11, 10, 20).unwrap();
    for i in 0..40 {
        t.write_vt(format!("DIR{i:03}\r\n").as_bytes());
    }

    for step in 0..12 {
        let (cols, rows) = if step % 2 == 0 {
            (40u16, 11u16)
        } else {
            (110, 38)
        };
        t.resize(cols, rows, 10, 20).unwrap();
        let snap = t.snapshot().unwrap();
        let mut counts: collections::HashMap<String, usize> = Default::default();
        for y in 0..snap.rows() {
            for tok in line_text(&snap, y).split_whitespace() {
                if (tok.starts_with("ROW") || tok.starts_with("DIR")) && tok.len() >= 6 {
                    *counts.entry(tok.to_string()).or_default() += 1;
                }
            }
        }
        let mut dups: Vec<_> = counts
            .iter()
            .filter(|&(_, &n)| n > 1)
            .map(|(k, n)| format!("{k}×{n}"))
            .collect();
        dups.sort();
        eprintln!(
            "[reflow-dup] step {step} {cols}x{rows}: {} tags, dups={dups:?}",
            counts.len()
        );
        assert!(
            dups.is_empty(),
            "viewport duplicated content after resize to {cols}x{rows}: {dups:?}"
        );
    }
}

#[test]
fn resize_shrink_does_not_double_full_width_padded_lines() {
    // Regression (remove-crosswords resize double-spacing / 错位). ConPTY pads
    // every line with trailing spaces out to the full console width. Without the
    // reflow trailing-space trim (vendored ghostty patch in
    // `libghostty-vt-sys/build.rs`), a column shrink wrapped that padding onto a
    // new row — each line became line+blank, ~doubling sb.total, which desynced
    // ConPTY's absolute cursor rows from the grid (input landed on history rows).
    // With the patch, padded lines must stay flat across a shrink, like plain
    // unpadded lines.
    let cols = 80u16;

    // Control: short lines, no trailing padding.
    let mut unpadded = GhosttyTerminal::new(cols, 24, 4000).unwrap();
    for i in 0..40 {
        unpadded.write_vt(format!("line{i:02}\r\n").as_bytes());
    }
    let unpadded_before = unpadded.snapshot().unwrap().scrollbar().total;
    unpadded.resize(cols - 2, 24, 10, 20).unwrap();
    let unpadded_after = unpadded.snapshot().unwrap().scrollbar().total;

    // Repro: every line padded with trailing spaces to the full width, then CRLF
    // (exactly what `Get-ChildItem`/`dir` output looks like through ConPTY).
    let mut padded = GhosttyTerminal::new(cols, 24, 4000).unwrap();
    for i in 0..40 {
        let body = format!("line{i:02}");
        let pad = cols as usize - body.len();
        padded.write_vt(format!("{body}{}\r\n", " ".repeat(pad)).as_bytes());
    }
    let padded_before = padded.snapshot().unwrap().scrollbar().total;
    padded.resize(cols - 2, 24, 10, 20).unwrap();
    let padded_after = padded.snapshot().unwrap().scrollbar().total;

    eprintln!(
        "[double-diag] unpadded {unpadded_before}->{unpadded_after} \
         padded {padded_before}->{padded_after}"
    );

    // Control must stay flat across the shrink.
    assert!(
        unpadded_after <= unpadded_before + 2,
        "unpadded lines should not grow on shrink: {unpadded_before}->{unpadded_after}"
    );
    // With the reflow trailing-space trim, the padded variant must also stay flat
    // (no line+blank doubling). Before the fix this was ~2× (41->81).
    assert!(
        padded_after <= padded_before + 2,
        "full-width padded lines must not bloat on shrink (reflow trailing-space \
         trim regressed?): {padded_before}->{padded_after}"
    );
}

#[cfg(windows)]
#[test]
fn grapheme_cluster_2027_enabled_matches_conhost() {
    // The terminal enables mode 2027 (grapheme clustering) by default on Windows in `new()`
    // to match ConPTY's permanent Graphemes mode. A ZWJ family emoji must then
    // measure 2 cols (clustered), not 6 (per-codepoint) — otherwise the cursor
    // misaligns against ConPTY on any line with such a cluster (resize or not).
    let mut t = GhosttyTerminal::new(80, 24, 1000).unwrap();
    for _ in 0..26 {
        t.write_vt("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}".as_bytes());
    }
    let snap = t.snapshot().unwrap();
    // 26 families × 2 cols = 52 → cursor stays on row 0 at col 52. Without
    // clustering it would be 26 × 6 = 156 cols and wrap to row 1.
    assert_eq!(
        snap.cursor().row.0,
        0,
        "clustered families (52 cols) fit one 80-col row"
    );
    assert_eq!(
        snap.cursor().col.0,
        52,
        "each ZWJ family advances 2 cols (mode 2027 on)"
    );
}

#[cfg(windows)]
#[test]
fn reflow_styled_trailing_matches_conhost() {
    // Difference 1 (conhost reflow alignment): conhost's MeasureRight trims trailing
    // spaces STYLE-BLIND; the 0001 patch now does too on Windows (dropped the
    // hasStyling guard). A line padded with bg-colored trailing spaces must stay
    // FLAT on a column shrink (like default padding), not wrap into blank rows.
    //
    // Repro of the bug for later work: this patch is Windows-gated, so on macOS/Linux
    // the styled variant still doubles; or `git revert` the styled-trim commit; or
    // run `reflow_styled_trailing_probe -- --ignored --nocapture` and compare
    // `styled` across platforms (41->41 on Windows, 41->81 elsewhere).
    let cols = 80u16;
    let build = |styled: bool| {
        let mut t = GhosttyTerminal::new(cols, 24, 8000).unwrap();
        for i in 0..40 {
            let body = format!("line{i:02}");
            let pad = " ".repeat(cols as usize - body.len());
            let line = if styled {
                format!("{body}\x1b[41m{pad}\x1b[0m\r\n")
            } else {
                format!("{body}{pad}\r\n")
            };
            t.write_vt(line.as_bytes());
        }
        let before = t.snapshot().unwrap().scrollbar().total;
        t.resize(40, 24, 10, 20).unwrap();
        (before, t.snapshot().unwrap().scrollbar().total)
    };
    let (_, default_after) = build(false);
    let (styled_before, styled_after) = build(true);
    assert!(
        default_after <= 43,
        "sanity: default-padded lines stay flat on shrink, got ->{default_after}"
    );
    assert!(
        styled_after <= styled_before + 2,
        "bg-colored trailing spaces must stay flat on shrink (0001 style-blind trim), \
         got {styled_before}->{styled_after} (81 means the hasStyling guard regressed)"
    );
}

#[cfg(windows)]
#[test]
fn resize_grow_preserves_cursor_row_on_windows() {
    // Regression for the scroll-after-resize 错位. conhost's ConPTY producer
    // (`SCREEN_INFORMATION::ResizeWithReflow`) preserves the cursor's offset
    // within the viewport on grow: the new rows appear as blanks BELOW the
    // prompt and the prompt stays "high". ghostty's default for a bottom cursor
    // is to "pull down" scrollback (the cursor pins to the new bottom), which
    // puts history where ConPTY expects blanks and smears ConPTY's viewport-
    // relative resize echoes onto a history row. The vendored Windows engine
    // preserves cursor y so Ghostty matches ConHost's cursor placement.
    let rows = 6u16;
    let mut t = GhosttyTerminal::new(40, rows, 4000).unwrap();
    // Fill past the viewport so there IS scrollback to (wrongly) pull down,
    // leaving the cursor on the bottom active row (no trailing newline → the
    // prompt sits at the bottom, like a shell after a command).
    for i in 0..12 {
        t.write_vt(format!("line{i:02}\r\n").as_bytes());
    }
    t.write_vt(b"PROMPT> ");

    let before = t.active_cursor_row().unwrap();
    assert_eq!(
        before,
        rows - 1,
        "cursor should start on the bottom active row"
    );

    // Grow the viewport (6 → 12 rows). With the patch the cursor row is
    // preserved (blanks below); without it ghostty pulls scrollback down and
    // pins the cursor to the new bottom (row 11).
    t.resize(40, 12, 10, 20).unwrap();
    let after = t.active_cursor_row().unwrap();
    assert_eq!(
        after, before,
        "grow must preserve the cursor's active row (conhost top-anchor); \
         got {after}, pull-down would be 11"
    );
}

#[test]
fn snapshot_scrollbar_reflects_scrollback() {
    // 3-row viewport, write 6 lines → 3 rows in scrollback.
    let mut t = GhosttyTerminal::new(20, 3, 100).unwrap();
    t.write_vt(b"l0\r\nl1\r\nl2\r\nl3\r\nl4\r\nl5");
    let sb = t.snapshot().unwrap().scrollbar();
    assert_eq!(sb.len, 3, "len = visible rows");
    assert!(sb.total >= 6, "total includes scrollback, got {}", sb.total);
    // At the bottom the viewport sits at the end: offset = total - len.
    assert_eq!(sb.offset, sb.total - sb.len, "at bottom offset = total-len");
    // Scrolled to the top, the offset is 0 (top-anchored).
    t.scroll_viewport_top();
    assert_eq!(t.scrollbar().offset, 0, "at top offset = 0");
}

#[test]
fn resize_grow_clamps_scroll_when_content_fits() {
    let mut t = GhosttyTerminal::new(20, 6, 100).unwrap();
    t.write_vt(b"l0\r\nl1\r\nl2\r\nl3\r\nl4\r\nl5");

    t.resize(20, 3, 10, 20).unwrap();
    let small = t.snapshot().unwrap().scrollbar();
    assert!(
        small.total > small.len,
        "precondition: small viewport scrolls"
    );

    t.resize(20, 8, 10, 20).unwrap();
    let snap = t.snapshot().unwrap();
    let grown = snap.scrollbar();
    assert!(
        grown.total <= grown.len,
        "grown viewport should not scroll when content fits: {grown:?}"
    );
    assert_eq!(line_text(&snap, 0), "l0");
    assert_eq!(line_text(&snap, 5), "l5");

    t.scroll_viewport_delta(1);
    assert_eq!(
        t.snapshot().unwrap().scrollbar(),
        grown,
        "scroll delta must no-op when content fits"
    );

    t.write_vt(b"\r\nl6\r\nl7\r\nl8");
    let overflow = t.snapshot().unwrap().scrollbar();
    assert!(
        overflow.total > overflow.len,
        "scrolling must return once content exceeds the viewport"
    );
}

#[test]
fn scroll_viewport_shows_scrollback() {
    // 3-row viewport, write 6 lines so 3 scroll into history.
    let mut t = GhosttyTerminal::new(20, 3, 100).unwrap();
    t.write_vt(b"l0\r\nl1\r\nl2\r\nl3\r\nl4\r\nl5");
    // At the bottom: newest lines visible.
    let bottom = line_text(&t.snapshot().unwrap(), 0);
    assert!(
        !bottom.starts_with("l0"),
        "bottom shows newest, got {bottom:?}"
    );

    // Scroll up: older lines come into view.
    t.scroll_viewport_delta(-3);
    let scrolled = line_text(&t.snapshot().unwrap(), 0);
    assert!(
        scrolled.starts_with("l0"),
        "scrolled top shows l0, got {scrolled:?}"
    );

    // Back to bottom.
    t.scroll_viewport_bottom();
    let back = line_text(&t.snapshot().unwrap(), 0);
    assert_eq!(back, bottom, "scroll-to-bottom restores the view");
}

#[test]
fn format_whole_screen_text() {
    let mut terminal = GhosttyTerminal::new(20, 3, 100).unwrap();
    terminal.write_vt(b"hello\r\nworld");
    let text = terminal.format_text(None, false, true).unwrap();
    assert!(text.contains("hello"), "got {text:?}");
    assert!(text.contains("world"), "got {text:?}");
}

#[test]
fn format_screen_range_reaches_scrollback() {
    // 2 visible rows, scrollback. Push the first line into history, then
    // extract it by SCREEN coordinate (0,0)..(4,0) → "first".
    let mut terminal = GhosttyTerminal::new(20, 2, 100).unwrap();
    terminal.write_vt(b"first\r\nsecond\r\nthird");
    // "first" is now in scrollback (SCREEN row 0); the viewport shows
    // "second"/"third". A SCREEN-coord range still extracts it.
    let text = terminal
        .format_screen_range((0, 0), (4, 0), false, false, true)
        .unwrap();
    assert_eq!(text.trim_end(), "first", "got {text:?}");
}

#[test]
fn viewport_grid_ref_resolves() {
    let mut terminal = GhosttyTerminal::new(20, 2, 100).unwrap();
    terminal.write_vt(b"x");
    // A valid viewport cell resolves to a non-null grid ref node.
    let r = terminal.viewport_grid_ref(0, 0).unwrap();
    assert!(!r.node.is_null());
}

#[test]
fn red_sgr_sets_foreground() {
    let mut terminal = GhosttyTerminal::new(8, 1, 100).unwrap();
    terminal.write_vt(b"\x1b[31mR");

    let snapshot = terminal.snapshot().unwrap();
    let style = snapshot.style(snapshot.cell(0, 0).style_id());
    // Ghostty's default palette red (SGR 31), flattened through the palette.
    assert_eq!(
        style.fg,
        AnsiColor::Spec(Color {
            r: 204,
            g: 102,
            b: 102
        })
    );
}

#[test]
fn rejects_zero_dimensions() {
    assert!(matches!(
        GhosttyTerminal::new(0, 24, 100),
        Err(Error::InvalidValue)
    ));
}

#[test]
fn wide_cjk_char_occupies_two_columns() {
    let mut terminal = GhosttyTerminal::new(8, 1, 100).unwrap();
    terminal.write_vt("中A".as_bytes());

    let snapshot = terminal.snapshot().unwrap();
    // Wide ideograph in column 0, spacer (no text) in column 1, narrow in 2.
    assert_eq!(snapshot.cell(0, 0).c(), '中');
    assert_eq!(snapshot.cell(2, 0).c(), 'A');
}

#[test]
fn mode_alt_screen_and_bracketed_paste() {
    let mut t = GhosttyTerminal::new(8, 3, 100).unwrap();
    assert!(!t.mode(mode::ALT_SCREEN));
    assert!(!t.mode(mode::BRACKETED_PASTE));
    t.write_vt(b"\x1b[?1049h\x1b[?2004h");
    assert!(t.mode(mode::ALT_SCREEN));
    assert!(t.mode(mode::BRACKETED_PASTE));
}

#[test]
fn mode_sgr_mouse() {
    let mut t = GhosttyTerminal::new(8, 1, 100).unwrap();
    t.write_vt(b"\x1b[?1000h\x1b[?1006h");
    assert!(t.mode(mode::MOUSE_NORMAL));
    assert!(t.mode(mode::MOUSE_SGR));
}

#[test]
fn shrink_resize_does_not_panic() {
    let mut t = GhosttyTerminal::new(80, 24, 1000).unwrap();
    for i in 0..200u32 {
        let line = format!("line {i} with some text that is fairly long to wrap\r\n");
        t.write_vt(line.as_bytes());
    }
    for (c, r) in [(60u16, 20u16), (40, 15), (20, 10), (5, 3), (1, 1), (80, 24)] {
        t.resize(c, r, 8, 16).unwrap();
        let _ = t.snapshot().unwrap();
    }
}

#[test]
fn custom_palette_applied() {
    let mut terminal = GhosttyTerminal::new(8, 1, 100).unwrap();
    let mut palette = [[0u8; 3]; 256];
    palette[1] = [10, 20, 30]; // SGR 31 resolves to palette index 1.
    terminal.set_colors([255, 255, 255], [0, 0, 0], [255, 255, 255], &palette);
    terminal.write_vt(b"\x1b[31mR");
    let snapshot = terminal.snapshot().unwrap();
    let style = snapshot.style(snapshot.cell(0, 0).style_id());
    assert_eq!(
        style.fg,
        AnsiColor::Spec(Color {
            r: 10,
            g: 20,
            b: 30
        })
    );
}

#[test]
fn write_pty_dsr_cursor_report() {
    let mut terminal = GhosttyTerminal::new(20, 5, 100).unwrap();
    // Move to row 3 col 4 (1-based 4;5) then request cursor position (DSR 6).
    terminal.write_vt(b"\x1b[4;5H\x1b[6n");
    let resp = terminal.take_pty_writes();
    assert_eq!(resp, b"\x1b[4;5R");
    // Draining is one-shot.
    assert!(terminal.take_pty_writes().is_empty());
}

#[test]
fn write_pty_primary_da() {
    let mut terminal = GhosttyTerminal::new(20, 5, 100).unwrap();
    terminal.write_vt(b"\x1b[c");
    assert!(!terminal.take_pty_writes().is_empty());
}

#[test]
fn bell_callback_counts() {
    let mut terminal = GhosttyTerminal::new(8, 1, 100).unwrap();
    terminal.write_vt(b"a\x07b\x07");
    assert_eq!(terminal.take_bell(), 2);
    assert_eq!(terminal.take_bell(), 0);
}

#[test]
fn title_poll_reports_change_once() {
    let mut terminal = GhosttyTerminal::new(8, 1, 100).unwrap();
    terminal.write_vt(b"\x1b]2;hello\x07");
    assert_eq!(terminal.poll_title().as_deref(), Some("hello"));
    // No further change → None.
    assert_eq!(terminal.poll_title(), None);
}

/// The `PWD` getter is populated by both the manual `PWD` setter and
/// by **OSC 7**. Upstream libghostty-vt dropped OSC 7 (an apprt action with no
/// apprt in headless builds); the vendored patch
/// `0003-pwd-store-osc7-headless.patch` routes `report_pwd` to
/// `Terminal.setPwd`, mirroring `.window_title` so direct setters and OSC 7
/// share the same observable state.
#[test]
fn pwd_set_via_setter_and_osc7() {
    // Setter → getter roundtrip works (the getter itself is fine).
    let t = GhosttyTerminal::new(8, 1, 100).unwrap();
    let p = b"/tmp/set";
    let s = VtString {
        ptr: p.as_ptr(),
        len: p.len(),
    };
    let rc = unsafe {
        ghostty_terminal_set(
            t.terminal,
            VtTerminalOption::PWD,
            (&s as *const VtString).cast(),
        )
    };
    assert_eq!(rc, VtResult::SUCCESS);
    assert_eq!(
        t.get_string(VtTerminalData::PWD),
        "/tmp/set",
        "the PWD setter populates the getter"
    );

    // OSC 7 populates the getter through report_pwd → setPwd.
    let mut t = GhosttyTerminal::new(8, 1, 100).unwrap();
    t.write_vt(b"\x1b]7;file:///home/u\x07");
    assert_eq!(
        t.get_string(VtTerminalData::PWD),
        "file:///home/u",
        "OSC 7 populates PWD"
    );
}

/// The headless build must process OSC 133 marks written via `write_vt` into per-row
/// SEMANTIC_PROMPT tags (OSC 7 needed a vendored patch for analogous plumbing).
#[test]
fn osc133_marks_tag_prompt_rows_headless() {
    let mut t = GhosttyTerminal::new(40, 4, 10_000).unwrap();
    // Row 0: prompt + echoed command. Row 1: command output. BEL-terminated,
    // matching the shipped pwsh integration.
    t.write_vt(b"\x1b]133;A\x07PS> \x1b]133;B\x07echo hi\r\n\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07");
    let tags = t.row_semantic_prompts().unwrap();
    assert_eq!(
        tags[0],
        VtRowSemanticPrompt::PROMPT,
        "row 0 (prompt+command) must be tagged PROMPT; got {tags:?}"
    );
    assert_eq!(
        tags[1],
        VtRowSemanticPrompt::NONE,
        "row 1 (output) must be untagged; got {tags:?}"
    );
}

/// Forwarded OSC 133 marks are zero-width state changes — they must not move the
/// cursor or add lines.
#[test]
fn osc133_marks_do_not_move_the_cursor() {
    let mut t = GhosttyTerminal::new(40, 5, 10_000).unwrap();
    t.write_vt(b"out\r\n");
    let row_before = t.active_cursor_row();
    let cursor_before = t.snapshot().unwrap().cursor();
    // A full prompt-render mark burst as forwarding emits it (;D always,
    // plus ;A/;B/;C in waterfall).
    t.write_vt(b"\x1b]133;D;0\x07\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07");
    assert_eq!(t.active_cursor_row(), row_before, "no line added");
    let cursor_after = t.snapshot().unwrap().cursor();
    assert_eq!(
        (cursor_after.col.0, cursor_after.row.0),
        (cursor_before.col.0, cursor_before.row.0),
        "marks are zero-width"
    );
}

/// OSC 7 updates the tracked working directory in headless mode.
#[test]
fn pwd_poll_reports_change() {
    let mut terminal = GhosttyTerminal::new(8, 1, 100).unwrap();
    // Canonical OSC 7 with empty authority (`file:///path`).
    terminal.write_vt(b"\x1b]7;file:///home/u\x07");
    let pwd = terminal.poll_pwd().expect("pwd reported");
    assert!(pwd.contains("/home/u"), "unexpected pwd: {pwd:?}");
    // No further change → None.
    assert_eq!(terminal.poll_pwd(), None);
}

#[test]
fn kitty_keyboard_flags_map_to_modes() {
    // The kitty keyboard protocol push (`CSI > flags u`) must surface in the
    // `Mode` facade so `session_key_flags` / the input path enable kitty press +
    // key-release encoding. This covers the gap where the flags lived
    // only in the engine's kitty stack and never reached vt_modes.
    use crate::terminal::Mode;
    let mut t = GhosttyTerminal::new(8, 1, 100).unwrap();
    assert!(
        t.kitty_keyboard_modes().is_empty(),
        "kitty protocol is inactive by default"
    );
    // Push disambiguate (1) + report-event-types (2).
    t.write_vt(b"\x1b[>3u");
    let m = t.kitty_keyboard_modes();
    assert!(m.contains(Mode::DISAMBIGUATE_ESC_CODES));
    assert!(m.contains(Mode::REPORT_EVENT_TYPES));
    assert!(!m.contains(Mode::REPORT_ALL_KEYS_AS_ESC));
    // Pop the flags → inactive again.
    t.write_vt(b"\x1b[<u");
    assert!(t.kitty_keyboard_modes().is_empty());
}

#[test]
fn crlf_output_appears_on_successive_rows() {
    let mut terminal = GhosttyTerminal::new(48, 4, 100).unwrap();
    terminal.write_vt(
        b"C:\\Workspace\\NiumaTerm>echo NiumaTerm\r\nNiumaTerm\r\nC:\\Workspace\\NiumaTerm>",
    );

    let snapshot = terminal.snapshot().unwrap();
    assert_eq!(
        line_text(&snapshot, 0),
        "C:\\Workspace\\NiumaTerm>echo NiumaTerm"
    );
    assert_eq!(line_text(&snapshot, 1), "NiumaTerm");
    assert_eq!(line_text(&snapshot, 2), "C:\\Workspace\\NiumaTerm>");
}

// ---- block-split harvest primitives (read_screen_row / track_screen_row) ----

fn row_read_text(row: &ScreenRowRead) -> String {
    row.cells.iter().map(|c| c.text.as_str()).collect()
}

/// Scrollback rows are readable without moving the viewport, and the soft-
/// wrap flag marks the logical-line join point.
#[test]
fn read_screen_row_reaches_scrollback_with_wrap_flag() {
    let mut t = GhosttyTerminal::new(10, 3, 100).unwrap();
    t.write_vt(b"0123456789ABC\r\n"); // soft-wraps: "0123456789" + "ABC"
    for i in 0..6 {
        t.write_vt(format!("line{i}\r\n").as_bytes());
    }

    // Rows 0-1 are now in scrollback; the viewport must not move to read them.
    let offset_before = t.scrollbar().offset;
    let row0 = t.read_screen_row(0).unwrap().expect("scrollback row 0");
    let row1 = t.read_screen_row(1).unwrap().expect("scrollback row 1");
    assert_eq!(t.scrollbar().offset, offset_before, "viewport untouched");

    assert_eq!(row_read_text(&row0), "0123456789");
    assert!(row0.wrapped, "row 0 soft-wraps into row 1");
    assert_eq!(row_read_text(&row1), "ABC");
    assert!(!row1.wrapped, "row 1 ends the logical line");
}

/// OSC 133 `;A` surfaces as `prompt_start`; OSC 8 spans surface with URIs.
#[test]
fn read_screen_row_prompt_tag_and_hyperlinks() {
    let mut t = GhosttyTerminal::new(30, 4, 100).unwrap();
    t.write_vt(b"\x1b]133;A\x07PS> \r\n");
    t.write_vt(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ text");

    let prompt_row = t.read_screen_row(0).unwrap().expect("prompt row");
    assert!(prompt_row.prompt_start, "OSC 133;A row tagged");

    let link_row = t.read_screen_row(1).unwrap().expect("link row");
    assert!(!link_row.prompt_start);
    assert_eq!(
        link_row.hyperlinks,
        vec![(0u16, 3u16, "https://example.com".to_string())]
    );
}

#[test]
fn read_screen_row_out_of_range_is_none() {
    let mut t = GhosttyTerminal::new(10, 3, 100).unwrap();
    t.write_vt(b"x");
    assert!(t.read_screen_row(9999).unwrap().is_none());
}
