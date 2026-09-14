use std::sync::mpsc;

use futures::executor::block_on;
use nmt_config::system::NewlineShortcut;
use nmt_input::keyboard::ModifiersState;

use crate::event::{BlockEvent, Msg};
use crate::ghostty::GhosttyTerminal;
use crate::input::{KeyPhase, TerminalKey};
use crate::pty_pipe::requests::answer_query;
use crate::selection::SelectionType;
use crate::session::interaction::{
    InputOutcome, TerminalInteraction, block_selection_span, selection_type_for_click_count,
};
use crate::session::state_tests::{session_from_engine, test_session};
use crate::session::{BlockPoint, TerminalSession};

fn frozen_session() -> (TerminalSession, GhosttyTerminal, mpsc::Receiver<Msg>) {
    let mut engine = GhosttyTerminal::new(24, 4, 100).unwrap();

    engine.write_vt(b"hello world");

    let handle = engine.finish_block().unwrap().unwrap();
    let (session, messages) = session_from_engine(&mut engine);

    session
        .block_store()
        .lock()
        .apply([BlockEvent::EngineBlock {
            seq: 1,
            handle,
            rows: 1,
        }]);

    (session, engine, messages)
}

fn answer_pending(engine: &mut GhosttyTerminal, messages: &mpsc::Receiver<Msg>) {
    while let Ok(Msg::Query(query)) = messages.try_recv() {
        answer_query(engine, 0, 0, query);
    }
}

#[test]
fn key_dispatch_reports_writes_and_requests_paste_from_the_host() {
    let (session, messages) = test_session();

    let mut interaction = TerminalInteraction::default();

    let snapshot = session.snapshot();

    let enter = TerminalKey {
        key: "enter",
        key_char: None,
        modifiers: ModifiersState::SHIFT,
        function: false,
        phase: KeyPhase::Press,
    };

    assert!(matches!(
        interaction.send_key(&session, &snapshot, &enter, NewlineShortcut::ShiftEnter),
        InputOutcome::Written
    ));
    assert!(matches!(messages.try_recv().unwrap(), Msg::Input(bytes) if bytes.as_ref() == b"\n"));

    session.mark_read_only();

    assert!(matches!(
        interaction.send_key(&session, &snapshot, &enter, NewlineShortcut::ShiftEnter),
        InputOutcome::Ignored
    ));

    #[cfg(target_os = "macos")]
    let modifiers = ModifiersState::SUPER;

    #[cfg(not(target_os = "macos"))]
    let modifiers = ModifiersState::CONTROL;

    let paste = TerminalKey {
        key: "v",
        key_char: None,
        modifiers,
        function: false,
        phase: KeyPhase::Press,
    };

    assert!(matches!(
        interaction.send_key(&session, &snapshot, &paste, NewlineShortcut::ShiftEnter),
        InputOutcome::PasteRequested
    ));
    assert!(messages.try_recv().is_err());
}

#[test]
fn copying_a_frozen_range_preserves_any_newer_pointer_selection() {
    for newer_pointer in [false, true] {
        let (session, mut engine, messages) = frozen_session();
        let mut interaction = TerminalInteraction::default();

        let start = BlockPoint {
            item: 0,
            line: 0,
            col: 1,
        };

        let end = BlockPoint { col: 3, ..start };

        interaction.begin_pointer();

        interaction.select_block(&session, start, SelectionType::Simple);

        assert!(interaction.block_selection().is_none());
        assert!(interaction.extend_block_selection(end));
        assert!(interaction.commit_block_selection());

        let snapshot = session.snapshot();
        let copy = interaction.copy_selection(&session, &snapshot).unwrap();

        answer_pending(&mut engine, &messages);

        assert_eq!(block_on(copy.request).unwrap().unwrap(), "ell");

        if newer_pointer {
            interaction.begin_pointer();

            interaction.select_block(&session, start, SelectionType::Simple);

            interaction.extend_block_selection(end);
        }

        interaction.complete_copy(&session, &snapshot, copy.completion);

        assert_eq!(
            interaction.block_selection(),
            newer_pointer.then_some((start, end))
        );
    }
}

#[test]
fn copy_during_word_expansion_reads_the_word_and_clears_only_that_gesture() {
    let (session, mut engine, messages) = frozen_session();
    let mut interaction = TerminalInteraction::default();

    interaction.begin_pointer();

    interaction.select_block(
        &session,
        BlockPoint {
            item: 0,
            line: 0,
            col: 7,
        },
        SelectionType::Semantic,
    );

    let snapshot = session.snapshot();
    let copy = interaction.copy_selection(&session, &snapshot).unwrap();

    answer_pending(&mut engine, &messages);

    assert_eq!(block_on(copy.request).unwrap().unwrap(), "world");

    interaction.poll_expansion(&session);

    let (start, end) = interaction.block_selection().unwrap();

    assert_eq!((start.col, end.col), (6, 10));

    interaction.complete_copy(&session, &snapshot, copy.completion);

    assert!(interaction.block_selection().is_none());
    assert!(interaction.copy_selection(&session, &snapshot).is_none());
}

#[test]
fn block_selection_spans_cover_intermediate_rows_and_clip_the_last_row() {
    let selection = Some((
        BlockPoint {
            item: 0,
            line: 1,
            col: 2,
        },
        BlockPoint {
            item: 1,
            line: 2,
            col: 4,
        },
    ));

    assert_eq!(block_selection_span(selection, 0, 0, 10), None);
    assert_eq!(block_selection_span(selection, 0, 1, 10), Some((2, 10)));
    assert_eq!(block_selection_span(selection, 1, 1, 10), Some((0, 10)));
    assert_eq!(block_selection_span(selection, 1, 2, 3), Some((0, 3)));
    assert_eq!(block_selection_span(selection, 1, 3, 10), None);
}

#[test]
fn repeated_clicks_choose_terminal_selection_modes() {
    assert_eq!(selection_type_for_click_count(1), SelectionType::Simple);
    assert_eq!(selection_type_for_click_count(2), SelectionType::Semantic);
    assert_eq!(selection_type_for_click_count(3), SelectionType::Lines);
    assert_eq!(selection_type_for_click_count(4), SelectionType::Lines);
}
