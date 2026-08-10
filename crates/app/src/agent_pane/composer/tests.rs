use nmt_agent_utils::chat::SlashCommandRunPolicy;

use crate::agent_pane::composer::{
    FileRestoreNext, RewindState, app_server, file_restore_next, restored_input_after_interruption,
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
