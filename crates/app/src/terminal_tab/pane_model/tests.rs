use std::sync::Arc;

use futures::executor::block_on;
use nmt_config::appearance::InputStyle;
use nmt_input::keyboard::ModifiersState;
use nmt_terminal::block_store::BlockStore;
use nmt_terminal::ghostty::ScrollbarInfo;
use nmt_terminal::input::{KeyPhase, TerminalKey, WheelDelta};
use nmt_terminal::selection::SelectionType;
use nmt_terminal::session::{
    SurfaceCell, SurfaceCellSide, SurfaceMouseButton, SurfaceMouseEventKind, SurfaceScreenCell,
};

use crate::terminal_tab::block_list::FrozenView;
use crate::terminal_tab::block_list::live::LiveItemState;
use crate::terminal_tab::block_list::reconcile::BlockListRenderMetrics;
use crate::terminal_tab::metrics::CellMetrics;
use crate::terminal_tab::pane_model::frame_record::FrameRecord;
use crate::terminal_tab::pane_model::key_action::{KeyOutcome, TextInput};
use crate::terminal_tab::pane_model::list_mirror::{BlockListMirror, ListOp, ListPosition};
use crate::terminal_tab::pane_model::mouse::{MouseInput, MouseOutcome};
use crate::terminal_tab::pane_model::scroll::ScrollOutcome;
use crate::terminal_tab::pane_model::selection_geometry::selection_drag_started;
use crate::terminal_tab::pane_model::test_session::{TestClipboard, assert_input, controller};
use crate::terminal_tab::pane_model::viewport::{LocalPoint, Viewport};

#[test]
fn terminal_requested_keyboard_modes_drive_keys_and_ime_commits() {
    let (mut model, input) = controller(b"\x1b[>31u", false);

    let mut key = TerminalKey {
        key: "a",
        key_char: Some("A"),
        modifiers: ModifiersState::SHIFT,
        function: false,
        phase: KeyPhase::Press,
    };

    assert!(matches!(model.key_down(&key), KeyOutcome::Written));

    key.phase = KeyPhase::Repeat;

    assert!(matches!(model.key_down(&key), KeyOutcome::Written));

    key.phase = KeyPhase::Release;
    key.key_char = None;

    assert!(matches!(model.send_key(&key), KeyOutcome::Written));
    assert!(matches!(model.send_key(&key), KeyOutcome::Ignored));
    assert!(model.write_text_input(TextInput::Commit("你好")));

    key.key = "v";

    key.modifiers = if cfg!(target_os = "macos") {
        ModifiersState::SUPER
    } else {
        ModifiersState::CONTROL
    };

    key.phase = KeyPhase::Press;

    assert!(matches!(model.key_down(&key), KeyOutcome::Ignored));

    key.phase = KeyPhase::Release;

    assert!(matches!(model.send_key(&key), KeyOutcome::Ignored));

    assert_input(
        &input,
        b"\x1b[97:65;2;65u\x1b[97:65;2:2;65u\x1b[97;2:3u\x1b[0;1;20320:22909u",
    );

    input.lock().clear();

    key.key = "c";
    key.modifiers = ModifiersState::CONTROL;
    key.phase = KeyPhase::Press;

    assert!(matches!(model.key_down(&key), KeyOutcome::Written));

    key.phase = KeyPhase::Release;

    assert!(matches!(model.send_key(&key), KeyOutcome::Written));

    assert_input(&input, b"\x1b[99;5u\x1b[99;5:3u");
}

#[test]
fn paste_uses_the_supplied_clipboard_and_respects_input_rejection() {
    let (mut model, input) = controller(b"\x1b[?2004h", false);

    let clipboard = TestClipboard::default();

    model.clipboard = Box::new(clipboard.clone());

    let paste = TerminalKey {
        key: "v",
        key_char: None,
        modifiers: if cfg!(target_os = "macos") {
            ModifiersState::SUPER
        } else {
            ModifiersState::CONTROL
        },
        function: false,
        phase: KeyPhase::Press,
    };

    assert!(matches!(model.send_key(&paste), KeyOutcome::Ignored));
    assert!(input.lock().is_empty());

    *clipboard.text.lock() = Some("clipboard text".into());

    assert!(matches!(model.send_key(&paste), KeyOutcome::Written));

    assert_input(&input, b"\x1b[200~clipboard text\x1b[201~");

    input.lock().clear();

    model.source.session.mark_read_only();

    assert!(matches!(model.send_key(&paste), KeyOutcome::Ignored));
    assert!(input.lock().is_empty());
}

#[test]
fn copy_failure_preserves_selection_and_success_preserves_a_newer_gesture() {
    for (reject_writes, newer_gesture) in [(true, false), (false, false), (false, true)] {
        let (mut model, _) = controller(b"hello world", false);

        let clipboard = TestClipboard {
            reject_writes,
            ..TestClipboard::default()
        };

        model.clipboard = Box::new(clipboard.clone());

        model.source.session.apply_screen_selection(
            SurfaceScreenCell { row: 0, col: 0 },
            SurfaceCellSide::Left,
            SurfaceMouseEventKind::Down,
            SelectionType::Simple,
        );

        model.source.session.apply_screen_selection(
            SurfaceScreenCell { row: 0, col: 4 },
            SurfaceCellSide::Right,
            SurfaceMouseEventKind::Move,
            SelectionType::Simple,
        );

        model.refresh_frame();

        let copy = model
            .interaction
            .copy_selection(&model.source.session, &model.source.snapshot)
            .unwrap();

        let text = block_on(copy.request).unwrap().unwrap();

        if newer_gesture {
            model.interaction.begin_pointer();
        }

        assert_eq!(
            model.finish_copy(text.clone(), copy.completion),
            !reject_writes
        );
        assert_eq!(
            clipboard.text.lock().as_ref(),
            (!reject_writes).then_some(&text)
        );
        assert_eq!(
            model
                .source
                .session
                .selection_range_in(&model.source.snapshot)
                .is_some(),
            reject_writes || newer_gesture
        );
    }
}

#[test]
fn resize_updates_content_geometry_and_only_invalidates_for_a_new_grid() {
    let (mut model, _) = controller(b"", false);

    let cell = model.cell_metrics.unwrap();

    assert!(!model.resize_content(327.0, 108.0, cell));
    assert_eq!(model.content_size, (327.0, 108.0));
    assert!(!model.frame_cache.needs_rebuild());
    assert!(model.resize_content(400.0, 144.0, cell));
    assert_eq!(model.content_cols(), 50);
    assert_eq!(model.content_size, (400.0, 144.0));
    assert_eq!(model.cell_metrics, Some(cell));
    assert!(model.frame_cache.needs_rebuild());
    assert!(!model.resize_content(400.0, 144.0, cell));
}

#[test]
fn pending_repaint_retains_shared_grid_coordinates_and_coalesces_wakes() {
    let (mut model, _) = controller(b"text", false);

    model.settings.input_style = InputStyle::FixedBottom;

    model.update_viewport();

    let cell = model.cell_metrics.unwrap();
    let offsets = model.viewport.row_offsets();

    assert_eq!(offsets.as_ref(), &[90.0; 6]);
    assert!(model.invalidate());
    assert!(!model.invalidate());
    assert!(Arc::ptr_eq(&offsets, &model.viewport.row_offsets()));
    assert_eq!(model.viewport.cursor_y(0, cell.height_px), offsets[0]);
    assert_eq!(
        model
            .viewport
            .cell_at(LocalPoint { x: 32.0, y: 90.0 }, cell)
            .0,
        SurfaceCell { col: 4, row: 0 }
    );
    assert!(model.frame_cache.current().is_some());

    model.begin_frame();

    assert_eq!(model.viewport.row_offsets(), offsets);
    assert!(model.invalidate());
}

#[test]
fn block_frame_reset_discards_visible_records_and_retains_live_origin() {
    let (mut model, _) = controller(b"", true);

    model.frozen.push_row(10.0, 3, 0, 40);

    model.frozen.push_separator(8.0);

    model.block_list.active_top = 90.0;

    model.begin_block_list_frame();

    assert!(model.frozen.row_top(3, 0).is_none());
    assert!(model.frozen.separators().is_empty());
    assert_eq!(model.viewport.cursor_y(0, 18.0), 90.0);

    let tail = FrozenView {
        active_top: 54.0,
        ..FrozenView::default()
    };

    let state = LiveItemState {
        in_flight: None,
        has_open_prompt: true,
    };

    let layout = state.layout(tail.active_top, 2, 18.0, 1.0);

    model.record_frame(FrameRecord::from_live_view(&tail, &layout, -18.0));

    assert_eq!(model.viewport.cursor_y(0, 18.0), 36.0);

    let chrome = &model.frozen.chrome()[0];

    assert_eq!(
        (chrome.top, chrome.bottom, chrome.header_y),
        (-18.0, 90.0, 36.0)
    );

    model.begin_block_list_frame();

    assert!(model.frozen.chrome().is_empty());
    assert_eq!(model.viewport.cursor_y(0, 18.0), 90.0);
}

#[test]
fn selection_drag_waits_for_quarter_cell_movement() {
    let origin = LocalPoint { x: 10.0, y: 10.0 };

    assert!(!selection_drag_started(
        origin,
        LocalPoint { x: 11.0, y: 11.0 },
        8.0
    ));
    assert!(selection_drag_started(
        origin,
        LocalPoint { x: 12.0, y: 10.0 },
        8.0
    ));
}

#[test]
fn both_viewports_map_pointer_cursor_and_thumb_consistently() {
    let cell = CellMetrics {
        width_px: 8.0,
        height_px: 18.0,
    };

    for viewport in [
        Viewport::Grid {
            scrollbar: ScrollbarInfo {
                total: 40,
                offset: 10,
                len: 20,
            },
            row_offsets: vec![36.0; 4].into(),
        },
        Viewport::BlockList {
            scroll_px: 10.0,
            max_scroll_px: 20.0,
            active_top: 36.0,
            viewport_px: 20.0,
        },
    ] {
        assert!(viewport.is_scrolled());
        assert_eq!(
            viewport.cell_at(LocalPoint { x: 16.0, y: 72.0 }, cell),
            (SurfaceCell { col: 2, row: 2 }, SurfaceCellSide::Left)
        );
        assert_eq!(
            viewport.cell_at(LocalPoint { x: 23.0, y: 72.0 }, cell).1,
            SurfaceCellSide::Right
        );
        assert_eq!(viewport.cursor_y(2, cell.height_px), 72.0);
        assert_eq!(viewport.thumb_target(0.5), Some(20.0));
    }
}

fn left_press(position: LocalPoint) -> MouseInput {
    MouseInput {
        position,
        button: Some(SurfaceMouseButton::Left),
        modifiers: ModifiersState::empty(),
        click_count: 1,
    }
}

#[test]
fn frozen_selection_obeys_mouse_reporting_and_drag_threshold() {
    for reporting in [false, true] {
        let (mut model, _) = controller(
            if reporting {
                b"\x1b[?1000h\x1b[?1006h"
            } else {
                b""
            },
            true,
        );

        model.frozen.begin_frame(54.0);

        model.frozen.push_row(0.0, 0, 0, 40);

        model.frozen.push_row(18.0, 0, 1, 40);

        model.update_viewport();

        assert!(matches!(
            (
                reporting,
                model.mouse_down(left_press(LocalPoint { x: 16.0, y: 4.0 }))
            ),
            (true, MouseOutcome::EngineHandled) | (false, MouseOutcome::FrozenSelectionStarted)
        ));
        assert_eq!(model.interaction.block_anchor().is_some(), !reporting);

        if !reporting {
            assert!(matches!(
                model.mouse_move(left_press(LocalPoint { x: 17.0, y: 4.0 })),
                MouseOutcome::Ignored
            ));
            assert!(model.interaction.block_selection().is_none());
            assert!(matches!(
                model.mouse_move(left_press(LocalPoint { x: 40.0, y: 20.0 })),
                MouseOutcome::SelectionChanged
            ));

            let (a, b) = model.interaction.block_selection().unwrap();

            assert_eq!((a.line, a.col, b.line, b.col), (0, 2, 1, 5));

            model.mouse_move(left_press(LocalPoint { x: 48.0, y: 120.0 }));

            assert_eq!(model.interaction.block_selection().unwrap().1.line, 1);

            model.mouse_up(left_press(LocalPoint { x: 40.0, y: 20.0 }));

            assert!(model.interaction.block_anchor().is_none());
            assert!(model.interaction.block_selection().is_some());
        }
    }
}

#[test]
fn modified_link_click_precedes_program_mouse_reporting() {
    let (mut model, _) = controller(b"https://example.com\x1b[?1000h", false);
    let mut input = left_press(LocalPoint { x: 40.0, y: 4.0 });

    #[cfg(target_os = "macos")]
    {
        input.modifiers = ModifiersState::SUPER;
    }

    #[cfg(not(target_os = "macos"))]
    {
        input.modifiers = ModifiersState::CONTROL;
    }

    assert!(
        matches!(model.mouse_down(input), MouseOutcome::OpenUrl(url) if url == "https://example.com")
    );
    assert!(
        model
            .source
            .session
            .selection_range_in(&model.source.session.snapshot())
            .is_none()
    );
    assert!(model.interaction.block_anchor().is_none());
}

#[test]
fn key_outcomes_distinguish_accepted_input_from_read_only_rejection() {
    let (mut model, _) = controller(b"", true);

    model.block_list.scrollbar = (24.0, 120.0);

    model.update_viewport();

    assert!(matches!(
        model.send_key(&TerminalKey {
            key: "escape",
            key_char: None,
            modifiers: ModifiersState::empty(),
            function: false,
            phase: KeyPhase::Press,
        }),
        KeyOutcome::Written
    ));
    assert!(
        model.viewport.is_scrolled(),
        "the host chooses when accepted input scrolls the view"
    );
    assert!(matches!(
        model.scroll_to_latest(),
        ScrollOutcome::List(ListOp::ScrollToEnd)
    ));
    assert!(!model.viewport.is_scrolled());
    assert!(matches!(model.scroll_to_latest(), ScrollOutcome::Ignored));

    model.source.session.mark_read_only();

    assert!(matches!(
        model.send_key(&TerminalKey {
            key: "escape",
            key_char: None,
            modifiers: ModifiersState::empty(),
            function: false,
            phase: KeyPhase::Press,
        }),
        KeyOutcome::Ignored
    ));
}

#[test]
fn list_mirror_plans_growth_eviction_remeasurement_and_scroll() {
    let mut mirror = BlockListMirror::default();

    let mut metrics = BlockListRenderMetrics {
        store_len: 2,
        evicted_items: 0,
        item_count: 3,
        frozen_px: 60.0,
        tail_px: 0.0,
        total_px: 100.0,
        offset_px: 0.0,
        last_item_px: 30.0,
    };

    let layout = (40, 18.0, 1.0);

    assert_eq!(
        mirror.sync(&metrics, layout, 2),
        [ListOp::Splice(0..1, 3), ListOp::Remeasure(1..3)]
    );
    assert!(mirror.sync(&metrics, layout, 2).is_empty());
    assert_eq!(
        mirror.sync(&metrics, (40, 18.0, 0.0), 2),
        [ListOp::RemeasureAll]
    );

    metrics.evicted_items = 1;
    metrics.store_len = 1;
    metrics.item_count = 2;

    assert_eq!(
        mirror.sync(&metrics, (40, 18.0, 0.0), 2),
        [ListOp::Splice(0..1, 0), ListOp::Remeasure(0..2)]
    );

    let store = BlockStore::default();

    assert_eq!(
        BlockListMirror::scroll_to_px(&store, 0, 2, layout, 10.0),
        ListOp::ScrollTo(ListPosition {
            item_ix: 0,
            offset_px: 10.0
        })
    );
    assert_eq!(
        BlockListMirror::scroll_to_px(&store, 0, 2, layout, 100.0),
        ListOp::ScrollToEnd
    );
}

#[test]
fn presentation_modules_do_not_import_host_services() {
    use std::fs;
    use std::path::Path;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/terminal_tab");

    let mut pending: Vec<_> = [
        "pane_model",
        "block_list",
        "frame",
        "frame_source",
        "wake.rs",
        "dirty.rs",
        "layout.rs",
        "scrollbar/geometry.rs",
    ]
    .into_iter()
    .map(|path| root.join(path))
    .collect();

    while let Some(path) = pending.pop() {
        assert!(
            path.exists(),
            "missing presentation source: {}",
            path.display()
        );

        if path.is_dir() {
            pending.extend(
                fs::read_dir(&path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path()),
            );

            continue;
        }

        let name = path.file_name().unwrap().to_string_lossy();

        if !name.ends_with(".rs") || name.contains("test") || name.contains("profile") {
            continue;
        }

        let text = fs::read_to_string(&path).unwrap();

        for line in text
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with("//"))
        {
            if path == root.join("frame/line.rs") && line == "use gpui::SharedString;" {
                continue;
            }

            assert!(
                !line.contains("gpui::")
                    && !line.contains("gpui_component")
                    && !line.contains("crate::terminal_tab::view")
                    && !line.contains("crate::terminal_tab::paint")
                    && !line.contains("rust_i18n::")
                    && !line.contains("active_colors"),
                "host dependency in {}: {line}",
                path.display()
            );
        }
    }
}

#[test]
fn end_scrolls_history_but_modified_end_and_alternate_screen_reach_the_pty() {
    for (vt, function, modifiers, scrolls) in [
        (&b""[..], false, ModifiersState::empty(), true),
        (&b""[..], true, ModifiersState::empty(), false),
        (&b""[..], false, ModifiersState::SHIFT, false),
        (&b"\x1b[?1049h"[..], false, ModifiersState::empty(), false),
    ] {
        let (mut model, _) = controller(vt, true);

        model.block_list.scrollbar = (24.0, 120.0);

        model.update_viewport();

        let outcome = model.key_down(&TerminalKey {
            key: "end",
            key_char: None,
            modifiers,
            function,
            phase: KeyPhase::Press,
        });

        match (scrolls, outcome) {
            (true, KeyOutcome::Scrolled(ScrollOutcome::List(ListOp::ScrollToEnd))) => {}
            (false, KeyOutcome::Written) => {}
            (_, outcome) => panic!("unexpected End outcome: {outcome:?}"),
        }
    }
}

#[test]
fn committed_text_is_sent_once_and_dropped_paths_use_bracketed_paste() {
    let (mut model, input) = controller(b"\x1b[?2004h", false);

    assert!(matches!(
        model.key_down(&TerminalKey {
            key: "a",
            key_char: Some("a"),
            modifiers: ModifiersState::empty(),
            function: false,
            phase: KeyPhase::Press,
        }),
        KeyOutcome::Ignored
    ));
    assert!(model.write_text_input(TextInput::Commit("a")));
    assert!(!model.write_text_input(TextInput::Commit("")));

    let paths = [
        "C:\\src\\main.rs".into(),
        "C:\\My Project\\notes.txt".into(),
    ];

    assert!(model.write_text_input(TextInput::DropPaths(&paths)));

    assert_input(
        &input,
        b"a\x1b[200~C:\\src\\main.rs \"C:\\My Project\\notes.txt\"\x1b[201~",
    );

    model.source.session.mark_read_only();

    assert!(!model.write_text_input(TextInput::Commit("rejected")));
    assert!(!model.write_text_input(TextInput::DropPaths(&paths)));
}

#[test]
fn hover_tracks_modifiers_wheel_and_pointer_exit_without_a_window() {
    let (mut model, _) = controller(b"https://example.com", false);
    let mut input = left_press(LocalPoint { x: 40.0, y: 4.0 });

    input.button = None;

    model.mouse_move(input);

    assert!(model.hovered_link().is_none());

    #[cfg(target_os = "macos")]
    let modifier = ModifiersState::SUPER;

    #[cfg(not(target_os = "macos"))]
    let modifier = ModifiersState::CONTROL;

    assert!(model.hover_modifiers_changed(modifier));
    assert_eq!(model.hovered_link().unwrap().url, "https://example.com");
    assert!(!model.hover_modifiers_changed(modifier));
    assert!(
        model
            .scroll_wheel(LocalPoint::default(), WheelDelta::Rows(0.0), modifier)
            .hover_changed
    );
    assert!(model.hovered_link().is_none());
    assert!(model.hover_modifiers_changed(modifier));
    assert!(model.pointer_left());
    assert!(!model.hover_modifiers_changed(modifier));

    model.refresh_frame();

    assert!(model.hovered_link().is_none());
}

#[test]
fn scrollbar_grab_preserves_offset_and_track_click_centers_the_thumb() {
    let (mut model, _) = controller(b"", true);

    model.content_size.1 = 100.0;
    model.block_list.scrollbar = (0.0, 100.0);

    model.update_viewport();

    assert!(matches!(
        model.scrollbar_mouse_down(LocalPoint { x: 0.0, y: 20.0 }, 0.0, 0.5),
        ScrollOutcome::Ignored
    ));
    assert!(model.scrollbar.is_dragging());
    assert!(matches!(
        model.mouse_move(left_press(LocalPoint { x: 0.0, y: 40.0 })),
        MouseOutcome::Scrolled(ScrollOutcome::List(_))
    ));
    assert!((model.block_list.scrollbar.0 - 40.0).abs() < 0.001);

    let release = model.mouse_up(left_press(LocalPoint { x: 0.0, y: 40.0 }));

    assert!(release.scrollbar_released);
    assert!(!model.scrollbar.is_dragging());
    assert!(
        !model
            .mouse_up(left_press(LocalPoint::default()))
            .scrollbar_released
    );
    assert!(matches!(
        model.scrollbar_mouse_down(LocalPoint { x: 0.0, y: 75.0 }, 0.0, 0.5),
        ScrollOutcome::List(_)
    ));
    assert_eq!(model.block_list.scrollbar.0, 100.0);
}
