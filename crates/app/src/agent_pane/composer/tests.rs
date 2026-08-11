use nmt_agent_utils::chat::SlashCommandRunPolicy;

use crate::agent_pane::composer::{
    CommandFeedbackKind, FileRestoreNext, RewindState, app_server, feedback_is_current,
    feedback_is_transient, file_restore_next, restored_input_after_interruption,
    rewind_blocks_submission, sessions, stream_json,
};

fn checkpoint() -> sessions::ClaudeCheckpoint {
    sessions::ClaudeCheckpoint {
        user_message_id: "00000000-0000-4000-8000-000000000001".into(),
        parent_message_id: None,
        prompt: "recover this prompt".into(),
        timestamp: Some("2026-08-07T01:00:00Z".into()),
        file_restore_availability: sessions::FileRestoreAvailability::Available,
    }
}

#[test]
fn picker_cancellation_and_processing_phases_are_distinct() {
    let picker_states = [
        RewindState::Loading { operation_id: 7 },
        RewindState::SelectingCheckpoint {
            operation_id: 7,
            checkpoints: vec![checkpoint()],
        },
        RewindState::SelectingAction {
            operation_id: 7,
            checkpoint: checkpoint(),
        },
    ];
    for state in &picker_states {
        assert!(state.is_picker());
        assert!(state.has_operation(7));
        assert!(!state.has_operation(6), "stale operations must be ignored");
        assert!(rewind_blocks_submission(Some(state)));
    }

    for state in [
        RewindState::RestoringFiles { operation_id: 7 },
        RewindState::ForkingConversation { operation_id: 7 },
    ] {
        assert!(!state.is_picker());
        assert!(rewind_blocks_submission(Some(&state)));
    }
    assert!(!rewind_blocks_submission(None));
}

#[test]
fn file_phase_success_and_failure_choose_the_safe_next_step() {
    assert_eq!(file_restore_next(false, Ok(())), FileRestoreNext::Complete);
    assert_eq!(
        file_restore_next(true, Ok(())),
        FileRestoreNext::ForkConversation
    );
    assert_eq!(
        file_restore_next(true, Err("expired checkpoint".into())),
        FileRestoreNext::RetryAction("expired checkpoint".into())
    );
}

#[test]
fn rewind_catalog_is_claude_only_and_idle_only() {
    let claude = stream_json::Session::adapter_commands();
    let rewind = claude
        .iter()
        .find(|command| command.name == "rewind")
        .expect("Claude rewind command");

    assert_eq!(rewind.run_policy, SlashCommandRunPolicy::IdleOnly);
    assert!(
        app_server::Session::adapter_commands()
            .iter()
            .all(|command| command.name != "rewind")
    );
}

#[test]
fn interrupted_prompt_returns_without_discarding_a_new_draft() {
    assert_eq!(
        restored_input_after_interruption("original prompt", ""),
        "original prompt"
    );
    assert_eq!(
        restored_input_after_interruption("original prompt", "new draft"),
        "original prompt\n\nnew draft"
    );
    assert_eq!(
        restored_input_after_interruption("original prompt", "original prompt"),
        "original prompt"
    );
}

/// A notice acknowledges a request before anything visible happens. Once the
/// command has run a whole turn, the transcript carries the real answer and
/// the acknowledgement is a line the user cannot dismiss, because only typing
/// clears it. Errors and the queued list are not acknowledgements.
#[test]
fn only_acknowledgements_retire_themselves() {
    assert!(feedback_is_transient(CommandFeedbackKind::Notice));
    assert!(!feedback_is_transient(CommandFeedbackKind::Error));
    assert!(!feedback_is_transient(CommandFeedbackKind::Queued));
}

/// A queued message counts commands still waiting. Several paths empty that
/// queue without going through the palette -- a failed spawn, an update
/// stopping active work, a conversation reset -- so the count has to retire
/// with the queue rather than wait for something to overwrite it.
#[test]
fn a_queued_message_does_not_outlive_its_queue() {
    assert!(feedback_is_current(CommandFeedbackKind::Queued, false));
    assert!(!feedback_is_current(CommandFeedbackKind::Queued, true));

    // The other kinds describe the command, not the queue, so an empty queue
    // says nothing about whether they still apply.
    assert!(feedback_is_current(CommandFeedbackKind::Error, true));
    assert!(feedback_is_current(CommandFeedbackKind::Notice, true));
}

/// Every message retires by one of three routes: it fades on its own, it
/// retires with what it describes, or it holds deliberately until something
/// replaces it. A kind belonging to none of them would sit above the composer
/// until the user happened to type, which is the bug this guards.
#[test]
fn every_message_kind_has_a_way_to_retire() {
    for kind in [
        CommandFeedbackKind::Notice,
        CommandFeedbackKind::Status,
        CommandFeedbackKind::Error,
        CommandFeedbackKind::Queued,
    ] {
        let fades_on_its_own = feedback_is_transient(kind);
        let retires_with_its_queue = !feedback_is_current(kind, /*queue_is_empty*/ true);
        let holds_until_replaced = matches!(
            kind,
            CommandFeedbackKind::Status | CommandFeedbackKind::Error
        );

        assert!(
            fades_on_its_own || retires_with_its_queue || holds_until_replaced,
            "a message kind with no way to retire was added"
        );
    }
}

/// Information the user asked for, and work still under way, must not fade
/// out from under them the way an acknowledgement does.
#[test]
fn requested_information_and_progress_hold() {
    assert!(!feedback_is_transient(CommandFeedbackKind::Status));
    assert!(feedback_is_current(
        CommandFeedbackKind::Status,
        /*queue_is_empty*/ true
    ));
}
