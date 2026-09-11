use std::hint::black_box;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, TestAppContext};
use nmt_agent::chat::{Item, ReplayItem, ReplayTurn};
use nmt_agent::transcript::TextField;
use nmt_config::agent::CollapseRows;

use crate::profile::AgentKind;
use crate::settings::AgentSettings;
use crate::transcript::TranscriptView;
use crate::transcript::reveal::RevealKey;

fn history(turns: u64) -> TranscriptView {
    let mut view = TranscriptView::new(AgentKind::Codex, None);

    for turn in 0..turns {
        view.push_stamped(
            turn,
            Item::UserMessage {
                text: Some("question".into()),
            },
        );

        view.push_stamped(
            turn,
            Item::AgentMessage {
                id: format!("reply-{turn}"),
                text: Some("answer".into()),
                questions: None,
            },
        );

        view.turn_ledger.settle_replayed(turn, None);
    }

    view.push_stamped(
        turns,
        Item::Reasoning {
            id: "live".into(),
            summary: Some("working".into()),
        },
    );

    view
}

fn assert_rows_match_rebuild(view: &mut TranscriptView, mode: CollapseRows) {
    view.refresh_rows(mode);

    let cached = view.rows.clone();
    let specs = view.build_row_specs(mode);

    view.sync_transcript_list(specs);

    assert_eq!(cached, view.rows);
}

#[test]
fn streaming_rebuilds_only_the_changed_turn() {
    let mut view = history(1_000);
    let mode = CollapseRows::WorkAndToolCalls;

    assert_rows_match_rebuild(&mut view, mode);

    for _ in 0..20 {
        assert!(view.append_delta("live", "more", TextField::ReasoningSummary));

        assert_rows_match_rebuild(&mut view, mode);

        assert_eq!(view.row_cache.rebuilt_entries, 1);
    }

    view.refresh_rows(mode);

    assert_eq!(view.row_cache.rebuilt_entries, 0);

    view.push_stamped(
        1_001,
        Item::UserMessage {
            text: Some("next".into()),
        },
    );

    assert_rows_match_rebuild(&mut view, mode);

    assert_eq!(view.row_cache.rebuilt_entries, 2);

    view.merge_completed(&Item::AgentMessage {
        id: "reply-500".into(),
        text: Some("updated answer".into()),
        questions: None,
    });

    assert_rows_match_rebuild(&mut view, mode);

    assert_eq!(view.row_cache.rebuilt_entries, 1_002);

    view.clear();

    assert!(!view.contains_item("live"));
    assert!(!view.append_delta("live", "stale", TextField::ReasoningSummary));

    assert_rows_match_rebuild(&mut view, mode);

    view.push_stamped(
        0,
        Item::Reasoning {
            id: "live".into(),
            summary: None,
        },
    );

    assert!(view.append_delta("live", "new conversation", TextField::ReasoningSummary));

    assert_rows_match_rebuild(&mut view, mode);
}

#[test]
fn indexed_updates_preserve_duplicate_id_order_and_item_kinds() {
    let mut view = history(0);

    view.push_stamped(
        0,
        Item::AgentMessage {
            id: "live".into(),
            text: None,
            questions: None,
        },
    );

    assert!(view.append_delta("live", "answer", TextField::Reply));

    view.merge_completed(&Item::AgentMessage {
        id: "live".into(),
        text: Some("complete".into()),
        questions: None,
    });

    assert!(
        matches!(&view.content.entries()[0].item, Item::Reasoning { summary: Some(text), .. } if text == "working")
    );
    assert!(
        matches!(&view.content.entries()[1].item, Item::AgentMessage { text: Some(text), .. } if text == "complete")
    );

    assert_rows_match_rebuild(&mut view, CollapseRows::Off);
}

#[gpui::test]
fn cached_rows_follow_disclosures_turns_and_mirrored_revisions(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(AgentSettings {
            reduce_motion: true,
            ..AgentSettings::default()
        });

        let entity = cx.new(|_| history(4));

        entity.update(cx, |view, cx| {
            for mode in [
                CollapseRows::Off,
                CollapseRows::ToolCalls,
                CollapseRows::WorkAndToolCalls,
            ] {
                assert_rows_match_rebuild(view, mode);
            }

            let mode = CollapseRows::WorkAndToolCalls;

            view.push_stamped(
                4,
                Item::Reasoning {
                    id: "next-work".into(),
                    summary: Some("more work".into()),
                },
            );

            view.start_working(cx);
            assert_rows_match_rebuild(view, mode);
            view.set_compacting(true, cx);
            assert_rows_match_rebuild(view, mode);
            view.set_compacting(false, cx);
            view.settle_turn(4, cx);
            assert_rows_match_rebuild(view, mode);

            for key in [RevealKey::Turn(4), RevealKey::Group(8), RevealKey::Row(8)] {
                view.toggle_disclosure(key, cx);
                assert_rows_match_rebuild(view, mode);
            }

            for key in [RevealKey::Row(8), RevealKey::Group(8), RevealKey::Turn(4)] {
                view.toggle_disclosure(key, cx);
                assert_rows_match_rebuild(view, mode);
            }

            view.mark_interrupted(4);
            assert_rows_match_rebuild(view, mode);
            view.discard_turn(4, cx);
            assert_rows_match_rebuild(view, mode);

            view.append_replay(
                5,
                ReplayTurn {
                    items: vec![ReplayItem {
                        item: Item::Reasoning {
                            id: "replayed".into(),
                            summary: Some("restored".into()),
                        },
                        at: None,
                    }],
                    ..ReplayTurn::default()
                },
                cx,
            );

            assert!(view.contains_item("replayed"));

            assert_rows_match_rebuild(view, mode);

            assert!(view.append_delta("replayed", " and extended", TextField::ReasoningSummary));

            assert_rows_match_rebuild(view, mode);

            let mirrored = [Item::Reasoning {
                id: "mirror".into(),
                summary: Some("first".into()),
            }];

            view.show_items(&mirrored, 1, cx);

            assert!(view.contains_item("mirror"));
            assert!(!view.contains_item("live"));

            assert_rows_match_rebuild(view, mode);
            view.clear();
            view.show_items(&mirrored, 1, cx);

            assert!(view.contains_item("mirror"));

            assert_rows_match_rebuild(view, mode);
        });
    });
}

#[test]
fn typed_edges_invalidate_cached_rows_until_the_reply_is_complete() {
    let mut view = history(10);

    view.push_stamped(
        10,
        Item::AgentMessage {
            id: "answer".into(),
            text: Some("hello".into()),
            questions: None,
        },
    );

    let mode = CollapseRows::Off;

    assert_rows_match_rebuild(&mut view, mode);

    assert!(view.append_delta("answer", "世界".repeat(100).as_str(), TextField::Reply));

    assert_rows_match_rebuild(&mut view, mode);

    let now = Instant::now();

    for elapsed in [16, 32, 80, 500, 2_000] {
        view.advance_typing(now + Duration::from_millis(elapsed));
        assert_rows_match_rebuild(&mut view, mode);
    }

    view.finish_typing();
    assert_rows_match_rebuild(&mut view, mode);
    view.refresh_rows(mode);

    assert_eq!(view.row_cache.rebuilt_entries, 0);
}

#[test]
#[ignore = "manual timing of long transcript updates"]
fn long_transcript_timing() {
    let mut view = history(5_000);
    let started = Instant::now();

    for _ in 0..200 {
        assert!(view.append_delta("live", "x", TextField::ReasoningSummary));

        let specs = view.build_row_specs(CollapseRows::WorkAndToolCalls);

        view.sync_transcript_list(specs);
        black_box(&view.rows);
    }

    let full = started.elapsed();
    let mut view = history(5_000);

    view.refresh_rows(CollapseRows::WorkAndToolCalls);

    let started = Instant::now();

    for _ in 0..200 {
        assert!(view.append_delta("live", "x", TextField::ReasoningSummary));

        view.refresh_rows(CollapseRows::WorkAndToolCalls);
        black_box(&view.rows);
    }

    eprintln!(
        "5000 turns, 200 tail updates: full={full:?}, incremental={:?}",
        started.elapsed()
    );
}
