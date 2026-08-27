use nmt_terminal::event::BlockEvent;
use nmt_terminal::ghostty::{BlockHandle, GhosttyTerminal};
use nmt_terminal::selection::SelectionRange;
use nmt_terminal::terminal::pos::{Column, Line, Pos};

use crate::block_list::*;
use crate::frame::line_from_parts;
use crate::theme;

fn row_texts(view: &FrozenView) -> Vec<String> {
    view.rows
        .iter()
        .map(|r| {
            r.line
                .text()
                .replace('\u{00a0}', " ")
                .trim_end()
                .to_string()
        })
        .collect()
}

fn finished_block(vt: &[u8], cols: u16, rows: u16) -> (GhosttyTerminal, HandleItemInfo) {
    let mut t = GhosttyTerminal::new(cols, rows, 10_000).unwrap();
    t.write_vt(vt);
    let handle = t.finish_block().unwrap().expect("block created");
    let rows = t.block_row_count(handle).unwrap();
    let info = HandleItemInfo {
        handle,
        rows,
        accent: theme::BLOCK_SUCCESS_COLOR,
        header: Some("cmd · ✓".into()),
    };
    (t, info)
}

/// A finished engine block renders as physical rows with chrome, item-
/// local geometry matching `item_px`, and `(id, generation, row)` shape
/// keys.
#[test]
fn frozen_block_view_reads_engine_rows() {
    let (t, info) = finished_block(b"hello\r\n\x1b[1mbold\r\n", 10, 4);
    assert_eq!(info.rows, 2);
    let (block, palette) = (
        t.block_acquire(info.handle).expect("acquire"),
        t.color_palette(),
    );

    let view = frozen_block_view(
        Some((&block, &palette)),
        &info,
        3,
        0..info.rows,
        10.0,
        ITEM_PAD_ROWS,
        None,
        Some(3),
    );
    assert_eq!(row_texts(&view), ["hello", "bold"]);
    assert_eq!(view.rows[0].y, 10.0, "content after the top pad row");
    assert_eq!(view.rows[1].y, 20.0);
    assert_eq!(view.active_top, 40.0, "rows + 2 pad rows");
    assert_eq!(view.separators, [0.0]);
    let chrome = &view.items_chrome[0];
    assert_eq!((chrome.item, chrome.top, chrome.bottom), (3, 0.0, 40.0));
    assert!(chrome.selected);
    assert!(view.rows[0].shape_key.is_some());
    assert_ne!(
        view.rows[0].shape_key, view.rows[1].shape_key,
        "per-row cache keys"
    );
    assert_eq!(view.rows[0].row, 0);

    // Styled reads carry through the visitor.
    assert!(view.rows[1].line.runs().iter().any(|r| r.bold));
}

/// Only the requested row range materializes; skipped head rows keep
/// their item-local y so geometry never shifts.
#[test]
fn frozen_block_view_windows_visible_rows() {
    let (t, info) = finished_block(b"r0\r\nr1\r\nr2\r\n", 10, 5);
    assert_eq!(info.rows, 3);
    let (block, palette) = (
        t.block_acquire(info.handle).expect("acquire"),
        t.color_palette(),
    );
    let view = frozen_block_view(
        Some((&block, &palette)),
        &info,
        0,
        1..2,
        10.0,
        ITEM_PAD_ROWS,
        None,
        None,
    );
    assert_eq!(row_texts(&view), ["r1"]);
    assert_eq!(view.rows[0].y, 20.0, "pad + one skipped row");
    assert_eq!(view.active_top, 50.0, "full item height regardless");
}

/// A stale/reflowing block (`None`) still renders chrome at the cached
/// height, so layout never jumps while content is briefly unavailable.
#[test]
fn frozen_block_view_placeholder_keeps_height() {
    let info = HandleItemInfo {
        handle: BlockHandle {
            id: 1,
            generation: 1,
        },
        rows: 4,
        accent: 0,
        header: None,
    };
    let view = frozen_block_view(None, &info, 0, 0..4, 10.0, ITEM_PAD_ROWS, None, None);
    assert!(view.rows.is_empty());
    assert_eq!(view.active_top, 60.0);
    assert_eq!(view.items_chrome.len(), 1);
}

/// Selection spans map straight onto physical rows.
#[test]
fn frozen_block_view_selection_spans_rows() {
    let (t, info) = finished_block(b"aaaa\r\nbbbb\r\ncccc\r\n", 10, 5);
    let (block, palette) = (
        t.block_acquire(info.handle).expect("acquire"),
        t.color_palette(),
    );
    let sel = Some((
        FrozenPoint {
            item: 0,
            line: 0,
            col: 2,
        },
        FrozenPoint {
            item: 0,
            line: 2,
            col: 1,
        },
    ));
    let view = frozen_block_view(
        Some((&block, &palette)),
        &info,
        0,
        0..info.rows,
        10.0,
        ITEM_PAD_ROWS,
        sel,
        None,
    );
    let spans: Vec<Option<(u16, u16)>> = view.rows.iter().map(|r| r.selected).collect();
    assert_eq!(
        spans,
        [Some((2, 10)), Some((0, 10)), Some((0, 2))],
        "endpoint rows partial, middle row full width"
    );
}

#[test]
fn frozen_selection_expands_wide_character() {
    let (t, info) = finished_block("中A".as_bytes(), 10, 2);
    let (block, palette) = (
        t.block_acquire(info.handle).expect("acquire"),
        t.color_palette(),
    );
    for col in [0, 1] {
        let point = FrozenPoint {
            item: 0,
            line: 0,
            col,
        };
        let view = frozen_block_view(
            Some((&block, &palette)),
            &info,
            0,
            0..info.rows,
            10.0,
            ITEM_PAD_ROWS,
            Some((point, point)),
            None,
        );

        assert_eq!(view.rows[0].selected, Some((0, 2)));
    }
}

/// Compact presentation (`pad_rows = 0`): rows start at the item top with
/// no pad, the item height is exactly its content rows, and adjacent
/// items pack contiguously — the classic-grid look over frozen blocks.
#[test]
fn compact_pad_rows_pack_rows_contiguously() {
    let (t, info) = finished_block(b"hello\r\nworld\r\n", 10, 4);
    let (block, palette) = (
        t.block_acquire(info.handle).expect("acquire"),
        t.color_palette(),
    );
    let view = frozen_block_view(
        Some((&block, &palette)),
        &info,
        0,
        0..info.rows,
        10.0,
        0.0,
        None,
        None,
    );
    assert_eq!(view.rows[0].y, 0.0, "no top pad");
    assert_eq!(view.rows[1].y, 10.0);
    assert_eq!(view.active_top, 20.0, "content rows only, no pads");

    let mut store = BlockStore::default();
    store.apply([BlockEvent::EngineBlock {
        seq: 1,
        handle: BlockHandle {
            id: 1,
            generation: 1,
        },
        rows: 2,
    }]);
    assert_eq!(item_px(&store.items()[0], 80, 10.0, 0.0), 20.0);
    assert_eq!(live_item_px(3, 2, 10.0, 0.0), 50.0, "history + live rows");

    let history = live_history_view(
        vec![(0u64, line_from_parts("a".into(), Vec::new(), Vec::new()))],
        1,
        10,
        10.0,
        0.0,
        None,
    );
    assert_eq!(history.rows[0].y, 0.0);
    assert_eq!(history.active_top, 10.0);
}

/// `frozen_selection_pieces` produces one per-block range with block-edge
/// endpoints resolved per item.
#[test]
fn selection_pieces_cover_block_ranges() {
    let mut store = BlockStore::default();
    store.apply([
        BlockEvent::EngineBlock {
            seq: 1,
            handle: BlockHandle {
                id: 6,
                generation: 1,
            },
            rows: 2,
        },
        BlockEvent::EngineBlock {
            seq: 2,
            handle: BlockHandle {
                id: 7,
                generation: 1,
            },
            rows: 5,
        },
    ]);
    let pieces = frozen_selection_pieces(
        &store,
        FrozenPoint {
            item: 0,
            line: 0,
            col: 2,
        },
        FrozenPoint {
            item: 1,
            line: 3,
            col: 4,
        },
    );
    assert_eq!(pieces.len(), 2);
    assert_eq!(pieces[0].handle.id, 6);
    assert_eq!(pieces[0].start, Some((0, 2)));
    assert_eq!(pieces[0].end, None, "selection continues past this item");
    assert_eq!(pieces[1].handle.id, 7);
    assert_eq!(pieces[1].start, None, "selection starts before this item");
    assert_eq!(pieces[1].end, Some((3, 4)));
}

/// `item_rows`/`item_px` use the cached engine row count.
#[test]
fn item_geometry_uses_cached_rows() {
    let mut store = BlockStore::default();
    store.apply([BlockEvent::EngineBlock {
        seq: 1,
        handle: BlockHandle {
            id: 1,
            generation: 1,
        },
        rows: 7,
    }]);
    let item = &store.items()[0];
    assert_eq!(item_rows(item, 80), 7);
    assert_eq!(
        item_px(item, 80, 10.0, ITEM_PAD_ROWS),
        90.0,
        "7 rows + 2 pad rows"
    );
    assert_eq!(
        live_item_px(3, 2, 10.0, ITEM_PAD_ROWS),
        70.0,
        "history + live + pads"
    );
}

/// The visible-row window clamps to the item and pads with overdraw.
#[test]
fn visible_rows_clamps_to_item() {
    // Item fully above the viewport (scrolled past): empty range.
    assert_eq!(
        visible_rows(-10_000.0, 50, 600.0, 10.0, ITEM_PAD_ROWS),
        50..50
    );
    // Item starting far below the viewport bottom: empty range.
    assert_eq!(visible_rows(10_000.0, 50, 600.0, 10.0, ITEM_PAD_ROWS), 0..0);
    // Item spanning the viewport: rows around the visible band only.
    let range = visible_rows(-1000.0, 1000, 600.0, 10.0, ITEM_PAD_ROWS);
    assert!(range.start > 0 && range.end < 1000);
    assert!(range.contains(&100), "row at viewport top included");
}

/// The hit map preserves whether a row belongs to a frozen block or the
/// live grid's absolute SCREEN history.
#[test]
fn hit_test_maps_block_list_points() {
    let mut hit = FrozenHitInfo::default();
    hit.push_row(10.0, 0, 0, 10); // item 0 row 0 at y=10
    hit.push_row(20.0, 0, 1, 10);
    hit.push_row(50.0, usize::MAX, 0, 10); // live-history sentinel
    hit.set_active_top(70.0);

    assert_eq!(
        hit.hit_test(35.0, 15.0, 10.0, 10.0, 10, ITEM_PAD_ROWS),
        Some(BlockListPoint::Frozen(FrozenPoint {
            item: 0,
            line: 0,
            col: 3
        }))
    );
    assert_eq!(
        hit.hit_test(15.0, 25.0, 10.0, 10.0, 10, ITEM_PAD_ROWS),
        Some(BlockListPoint::Frozen(FrozenPoint {
            item: 0,
            line: 1,
            col: 1
        }))
    );
    // Beyond the row width clamps to the last column.
    assert_eq!(
        hit.hit_test(500.0, 15.0, 10.0, 10.0, 10, ITEM_PAD_ROWS),
        Some(BlockListPoint::Frozen(FrozenPoint {
            item: 0,
            line: 0,
            col: 9
        }))
    );
    assert_eq!(
        hit.hit_test(0.0, 55.0, 10.0, 10.0, 10, ITEM_PAD_ROWS),
        Some(BlockListPoint::LiveHistory { row: 0, col: 0 }),
        "live history keeps its SCREEN row"
    );
    assert_eq!(
        hit.hit_test(0.0, 5.0, 10.0, 10.0, 10, ITEM_PAD_ROWS),
        None,
        "above rows"
    );
}

/// Chrome accents and headers key off the metadata.
#[test]
fn chrome_keys_off_metadata() {
    let mut store = BlockStore::default();
    store.apply([
        BlockEvent::EngineBlock {
            seq: 1,
            handle: BlockHandle {
                id: 1,
                generation: 1,
            },
            rows: 2,
        },
        BlockEvent::EngineBlock {
            seq: 2,
            handle: BlockHandle {
                id: 2,
                generation: 1,
            },
            rows: 1,
        },
    ]);
    let t0 = time::UNIX_EPOCH;
    store.update_meta(1, |m| {
        m.command = Some("build".into());
        m.exit_code = Some(0);
        m.started_at = Some(t0);
        m.ended_at = Some(t0 + time::Duration::from_secs(2));
    });
    store.update_meta(2, |m| {
        m.command = Some("bad".into());
        m.exit_code = Some(127);
        m.ended_at = Some(t0 + time::Duration::from_secs(2));
    });

    let info1 = handle_item_info(&store.items()[0]).unwrap();
    assert_eq!(info1.accent, theme::BLOCK_SUCCESS_COLOR);
    assert_eq!(info1.header.as_deref(), Some("build · ✓ 2.0s"));
    let info2 = handle_item_info(&store.items()[1]).unwrap();
    assert_eq!(info2.accent, theme::BLOCK_FAILURE_COLOR);
    assert_eq!(info2.header.as_deref(), Some("bad · ✗ 127"));
}

#[test]
fn item_header_waits_for_end_time() {
    let t0 = time::UNIX_EPOCH;
    let mut meta = SegmentMeta {
        command: Some("build".into()),
        started_at: Some(t0),
        ..SegmentMeta::default()
    };

    assert_eq!(item_header(&meta), None);

    meta.ended_at = Some(t0 + time::Duration::from_secs(2));
    assert_eq!(item_header(&meta).as_deref(), Some("build · ? · 2.0s"));
}

/// Previous/next navigation walks item tops with edge no-ops.
#[test]
fn nav_item_top_walks_items() {
    let mut store = BlockStore::default();
    store.apply([
        BlockEvent::EngineBlock {
            seq: 1,
            handle: BlockHandle {
                id: 1,
                generation: 1,
            },
            rows: 1,
        },
        BlockEvent::EngineBlock {
            seq: 2,
            handle: BlockHandle {
                id: 2,
                generation: 1,
            },
            rows: 2,
        },
        BlockEvent::EngineBlock {
            seq: 3,
            handle: BlockHandle {
                id: 3,
                generation: 1,
            },
            rows: 1,
        },
    ]);
    // Heights: 30, 40, 30 → tops 0, 30, 70.
    assert_eq!(
        nav_item_top(&store, 80, 10.0, ITEM_PAD_ROWS, 0.0, 1),
        Some(30.0)
    );
    assert_eq!(
        nav_item_top(&store, 80, 10.0, ITEM_PAD_ROWS, 30.0, 1),
        Some(70.0)
    );
    assert_eq!(nav_item_top(&store, 80, 10.0, ITEM_PAD_ROWS, 70.0, 1), None);
    assert_eq!(
        nav_item_top(&store, 80, 10.0, ITEM_PAD_ROWS, 70.0, -1),
        Some(30.0)
    );
    assert_eq!(nav_item_top(&store, 80, 10.0, ITEM_PAD_ROWS, 0.0, -1), None);
}

#[test]
fn live_chrome_hides_running_header() {
    let chrome = live_chrome(3, 2, 10.0, true, true).unwrap();
    assert_eq!((chrome.item, chrome.top, chrome.bottom), (3, 0.0, 20.0));
    assert_eq!(chrome.accent, theme::BLOCK_RUNNING_COLOR);
    assert_eq!(chrome.header, None);
    assert!(chrome.selected);

    assert!(live_chrome(3, 0, 10.0, true, false).is_none());
}

#[test]
fn live_chrome_marks_idle_prompt() {
    let chrome = live_chrome(2, 3, 10.0, false, true).unwrap();
    assert_eq!((chrome.item, chrome.top, chrome.bottom), (2, 0.0, 30.0));
    assert_eq!(chrome.accent, theme::BLOCK_INPUT_COLOR);
    assert_eq!(chrome.header, None);
    assert!(chrome.selected);

    assert!(live_chrome(2, 0, 10.0, false, false).is_none());
}

/// The live-history view positions SCREEN rows, applies the engine
/// selection, and reports the active top.
#[test]
fn live_history_view_positions_rows() {
    let lines = vec![
        (0u64, line_from_parts("a".into(), Vec::new(), Vec::new())),
        (2u64, line_from_parts("c".into(), Vec::new(), Vec::new())),
    ];
    let selection = SelectionRange::new(
        Pos::new(Line(0), Column(2)),
        Pos::new(Line(2), Column(3)),
        false,
    );
    let view = live_history_view(lines, 3, 10, 10.0, ITEM_PAD_ROWS, Some(selection));
    assert_eq!(view.rows.len(), 2);
    assert_eq!(view.rows[0].y, 10.0, "pad + row 0");
    assert_eq!(view.rows[1].y, 30.0, "pad + row 2 (row 1 not visible)");
    assert_eq!(view.rows[0].item, usize::MAX, "live-history sentinel");
    assert_eq!(view.rows[0].selected, Some((2, 10)));
    assert_eq!(view.rows[1].selected, Some((0, 4)));
    assert_eq!(view.active_top, 40.0, "pad + total history rows");
}

/// Frozen Kitty direct read: a placement frozen into a
/// block reports a block-relative row, its pixels read back lazily, and
/// the paint mapping lands on the right visible row band.
#[test]
fn frozen_block_images_map_visible_rows() {
    use crate::graphics::{ReleaseQueue, graphic_to_generation};

    let mut t = GhosttyTerminal::new(20, 5, 10_000).unwrap();
    t.resize(20, 5, 10, 20).unwrap(); // cell pixel size for grid math
    t.write_vt(b"a\r\nb\r\n");
    t.write_vt(b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/wAA/w==\x1b\\");
    let handle = t.finish_block().unwrap().expect("block created");
    let block = t.block_acquire(handle).expect("acquire");

    let placements = t.block_placements(&block);
    assert_eq!(placements.len(), 1, "one frozen placement");
    let p = placements[0];
    assert_eq!(p.image_id, 1);
    assert_eq!((p.screen_col, p.screen_row), (0, 2), "block-relative row");
    assert!(p.grid_cols >= 1 && p.grid_rows >= 1);

    let data = t.block_image_pixels(&block, 1).expect("frozen pixels");
    assert_eq!((data.width, data.height), (1, 1));
    assert!(t.block_image_pixels(&block, 999).is_none(), "unknown id");

    let q: ReleaseQueue = Default::default();
    let generation = graphic_to_generation(data, &q).unwrap();
    let mut generations = collections::HashMap::new();
    generations.insert(1u32, generation);

    let images = frozen_block_images(&placements, &generations, &(0..3), 10.0, ITEM_PAD_ROWS);
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].y, 10.0 + 2.0 * 10.0, "pad + block row 2");
    assert_eq!((images[0].col, images[0].width), (0, p.grid_cols));

    // Rows outside the visible window materialize nothing.
    assert!(
        frozen_block_images(&placements, &generations, &(0..2), 10.0, ITEM_PAD_ROWS).is_empty()
    );
    // A missing generation is skipped (retry next frame), not painted.
    assert!(
        frozen_block_images(
            &placements,
            &Default::default(),
            &(0..3),
            10.0,
            ITEM_PAD_ROWS
        )
        .is_empty()
    );
}

/// Kitty V1 per-block ownership intentionally differs from active-screen ownership:
/// cross-block place-by-id falls flat on the fresh
/// screen, and an active delete-all cannot reach a frozen block's images.
#[test]
fn kitty_v1_per_block_ownership_deviations() {
    let mut t = GhosttyTerminal::new(20, 5, 10_000).unwrap();
    t.resize(20, 5, 10, 20).unwrap();
    t.write_vt(b"\x1b_Ga=T,f=32,s=1,v=1,i=7;/wAA/w==\x1b\\");
    let frozen = t.finish_block().unwrap().expect("block created");

    // Cross-block place-by-id: the new screen's storage is empty, so a
    // A placement-only command references nothing; a future implementation could
    // forward the image definition table if this pattern matters).
    t.write_vt(b"\x1b_Ga=p,i=7\x1b\\");
    assert!(!t.kitty_image_exists(7), "new screen storage starts empty");

    // Active delete-all only touches active storage so frozen blocks remain immutable:
    // frozen block keeps showing its freeze-time pixels.
    t.write_vt(b"\x1b_Ga=d\x1b\\");
    let block = t.block_acquire(frozen).expect("acquire");
    assert!(
        t.block_image_pixels(&block, 7).is_some(),
        "frozen pixels survive an active delete-all"
    );
    assert_eq!(t.block_placements(&block).len(), 1);
}
