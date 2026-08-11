use std::time::{Duration, SystemTime};

use crate::background_task::{
    BackgroundTaskDiscoveryState, BackgroundTaskKey, BackgroundTaskRefs, BackgroundTaskRegistry,
    BackgroundTaskState, BackgroundTaskUpdate,
};

fn registry() -> BackgroundTaskRegistry {
    BackgroundTaskRegistry::new(BackgroundTaskKey::codex("parent-thread"))
}

#[test]
fn same_local_id_from_two_providers_stays_distinct() {
    let mut registry = registry();
    registry.apply(
        BackgroundTaskKey::codex("shared-id"),
        BackgroundTaskUpdate::state(BackgroundTaskState::Working),
    );
    registry.apply(
        BackgroundTaskKey::claude_code("shared-id"),
        BackgroundTaskUpdate::state(BackgroundTaskState::Done),
    );

    let snapshot = registry.snapshot();
    assert_eq!(snapshot.tasks.len(), 2);
    assert_eq!(snapshot.active_count(), 1);
    assert_eq!(snapshot.terminal_count(), 1);
}

#[test]
fn a_row_without_optional_metadata_stays_visible_with_a_derived_name() {
    let mut registry = registry();
    let key = BackgroundTaskKey::codex("thread-01H9ZQF4");
    registry.apply(
        key.clone(),
        BackgroundTaskUpdate::state(BackgroundTaskState::Working),
    );

    let summary = registry.get(&key).expect("row exists");
    assert!(summary.display_name.is_none());
    assert!(summary.objective.is_none());
    assert_eq!(summary.display_label(), "Agent 01H9ZQF4");

    registry.apply(
        key.clone(),
        BackgroundTaskUpdate {
            display_name: Some("Reviewer".into()),
            ..BackgroundTaskUpdate::default()
        },
    );
    assert_eq!(
        registry.get(&key).expect("row exists").display_label(),
        "Reviewer"
    );
}

#[test]
fn lifecycle_states_group_into_running_and_finished() {
    let running = [
        BackgroundTaskState::Starting,
        BackgroundTaskState::Working,
        BackgroundTaskState::NeedsInput,
    ];
    let finished = [
        BackgroundTaskState::Done,
        BackgroundTaskState::Interrupted,
        BackgroundTaskState::Stopped,
        BackgroundTaskState::Failed,
    ];
    assert!(running.iter().all(|state| state.is_active()));
    assert!(finished.iter().all(|state| state.is_terminal()));

    let mut registry = registry();
    for (index, state) in running.iter().chain(finished.iter()).enumerate() {
        registry.apply(
            BackgroundTaskKey::codex(format!("child-{index}")),
            BackgroundTaskUpdate::state(*state),
        );
    }

    let snapshot = registry.snapshot();
    assert_eq!(snapshot.active_count(), running.len());
    assert_eq!(snapshot.terminal_count(), finished.len());
    assert_eq!(snapshot.needs_input_count(), 1);
}

#[test]
fn an_explicit_update_after_a_terminal_state_resumes_the_task() {
    let mut registry = registry();
    let key = BackgroundTaskKey::claude_code("task-1");
    registry.apply(
        key.clone(),
        BackgroundTaskUpdate::state(BackgroundTaskState::Done),
    );
    assert_eq!(registry.snapshot().active_count(), 0);

    registry.apply(
        key.clone(),
        BackgroundTaskUpdate::state(BackgroundTaskState::Working),
    );
    assert_eq!(
        registry.get(&key).expect("row exists").state,
        BackgroundTaskState::Working
    );
    assert_eq!(registry.snapshot().active_count(), 1);
}

#[test]
fn activity_advances_on_creation_and_lifecycle_change_only() {
    let mut registry = registry();
    let key = BackgroundTaskKey::codex("child-1");

    registry.apply(
        key.clone(),
        BackgroundTaskUpdate::state(BackgroundTaskState::Starting),
    );
    let after_create = registry.snapshot().activity;
    assert!(after_create > 0);

    registry.apply(
        key.clone(),
        BackgroundTaskUpdate {
            status: Some("reading files".into()),
            ..BackgroundTaskUpdate::default()
        },
    );
    assert_eq!(registry.snapshot().activity, after_create);

    registry.apply(
        key,
        BackgroundTaskUpdate::state(BackgroundTaskState::Working),
    );
    assert_eq!(registry.snapshot().activity, after_create + 1);
}

#[test]
fn repeating_a_known_state_changes_nothing() {
    let mut registry = registry();
    let key = BackgroundTaskKey::codex("child-1");
    registry.apply(
        key.clone(),
        BackgroundTaskUpdate::state(BackgroundTaskState::Working),
    );
    let baseline = registry.snapshot();

    assert!(!registry.apply(
        key,
        BackgroundTaskUpdate::state(BackgroundTaskState::Working)
    ));
    assert_eq!(registry.snapshot(), baseline);
}

#[test]
fn a_delayed_restored_row_cannot_replace_a_newer_live_state() {
    let mut registry = registry();
    let key = BackgroundTaskKey::codex("child-1");
    let starting_sequence = registry.sequence();

    registry.apply(
        key.clone(),
        BackgroundTaskUpdate::state(BackgroundTaskState::Done),
    );
    registry.merge_restored(
        key.clone(),
        BackgroundTaskUpdate {
            state: Some(BackgroundTaskState::Working),
            objective: Some("review the diff".into()),
            ..BackgroundTaskUpdate::default()
        },
        starting_sequence,
    );

    let summary = registry.get(&key).expect("row exists");
    assert_eq!(summary.state, BackgroundTaskState::Done);
    assert_eq!(summary.objective.as_deref(), Some("review the diff"));
}

#[test]
fn a_restored_row_creates_a_task_that_live_updates_never_reported() {
    let mut registry = registry();
    let key = BackgroundTaskKey::codex("child-restored");
    let starting_sequence = registry.sequence();

    registry.merge_restored(
        key.clone(),
        BackgroundTaskUpdate {
            state: Some(BackgroundTaskState::Done),
            refs: Some(BackgroundTaskRefs::Codex {
                thread_id: "child-restored".into(),
                parent_thread_id: Some("parent-thread".into()),
            }),
            ..BackgroundTaskUpdate::default()
        },
        starting_sequence,
    );

    let summary = registry.get(&key).expect("row exists");
    assert_eq!(summary.state, BackgroundTaskState::Done);
    assert_eq!(
        summary.refs,
        BackgroundTaskRefs::Codex {
            thread_id: "child-restored".into(),
            parent_thread_id: Some("parent-thread".into()),
        }
    );
}

#[test]
fn the_earliest_known_start_time_wins() {
    let mut registry = registry();
    let key = BackgroundTaskKey::codex("child-1");
    let late = SystemTime::UNIX_EPOCH + Duration::from_secs(200);
    let early = SystemTime::UNIX_EPOCH + Duration::from_secs(100);

    registry.apply(
        key.clone(),
        BackgroundTaskUpdate {
            started_at: Some(late),
            ..BackgroundTaskUpdate::default()
        },
    );
    registry.apply(
        key.clone(),
        BackgroundTaskUpdate {
            started_at: Some(early),
            ..BackgroundTaskUpdate::default()
        },
    );

    assert_eq!(
        registry.get(&key).expect("row exists").started_at,
        Some(early)
    );
}

#[test]
fn discovery_state_only_reports_real_transitions() {
    let mut registry = registry();
    assert_eq!(
        registry.discovery(),
        &BackgroundTaskDiscoveryState::NotLoaded
    );
    assert!(registry.set_discovery(BackgroundTaskDiscoveryState::Loading));
    assert!(!registry.set_discovery(BackgroundTaskDiscoveryState::Loading));
    assert!(registry.set_discovery(BackgroundTaskDiscoveryState::Ready));
}

mod child_transcript {
    use crate::background_task::{
        BackgroundTaskTranscript, BackgroundTaskTranscriptState, BackgroundTaskTranscriptUpdate,
        MAX_TRANSCRIPT_ITEMS,
    };
    use crate::chat::Item;

    fn message(id: &str, text: &str) -> Item {
        Item::AgentMessage {
            id: id.into(),
            text: Some(text.into()),
        }
    }

    #[test]
    fn a_completed_item_folds_into_the_entry_that_streamed_it() {
        let mut transcript = BackgroundTaskTranscript::default();
        transcript.push(Item::CommandExecution {
            id: "cmd-1".into(),
            command: "cargo test".into(),
            purpose: Some("Run focused tests".into()),
            aggregated_output: Some("streamed".into()),
            status: Some("inProgress".into()),
            exit_code: None,
        });
        transcript.push(Item::CommandExecution {
            id: "cmd-1".into(),
            command: "cargo test".into(),
            purpose: None,
            aggregated_output: None,
            status: Some("completed".into()),
            exit_code: Some(0),
        });

        assert_eq!(transcript.items().len(), 1, "a completion is not a new row");
        let Item::CommandExecution {
            purpose,
            status,
            exit_code,
            ..
        } = &transcript.items()[0]
        else {
            panic!("expected the command row");
        };
        assert_eq!(purpose.as_deref(), Some("Run focused tests"));
        assert_eq!(status.as_deref(), Some("completed"));
        assert_eq!(*exit_code, Some(0));
    }

    #[test]
    fn the_oldest_items_are_dropped_at_the_retention_bound() {
        let mut transcript = BackgroundTaskTranscript::default();
        for index in 0..MAX_TRANSCRIPT_ITEMS + 10 {
            transcript.push(message(&format!("m{index}"), "line"));
        }

        assert_eq!(transcript.items().len(), MAX_TRANSCRIPT_ITEMS);
        assert_eq!(transcript.dropped(), 10);
        assert_eq!(
            transcript.items()[0].id(),
            Some("m10"),
            "the surviving window ends at the newest item"
        );
    }

    #[test]
    fn a_conversation_within_the_bound_reports_nothing_dropped() {
        let mut transcript = BackgroundTaskTranscript::default();
        transcript.extend([message("a", "one"), message("b", "two")]);

        assert_eq!(transcript.dropped(), 0);
        assert_eq!(transcript.items().len(), 2);
    }

    #[test]
    fn a_provider_read_replaces_the_conversation_and_clears_the_dropped_count() {
        let mut transcript = BackgroundTaskTranscript::default();
        for index in 0..MAX_TRANSCRIPT_ITEMS + 5 {
            transcript.push(message(&format!("m{index}"), "line"));
        }
        assert!(transcript.dropped() > 0);

        transcript.replace(vec![message("only", "complete read")]);

        assert_eq!(transcript.items().len(), 1);
        assert_eq!(
            transcript.dropped(),
            0,
            "nothing is missing from a complete read"
        );
    }

    #[test]
    fn restored_history_fills_an_empty_child_but_never_replaces_live_content() {
        let mut empty = BackgroundTaskTranscript::default();
        assert!(empty.restore(vec![message("h1", "from history")]));
        assert_eq!(empty.items().len(), 1);

        let mut live = BackgroundTaskTranscript::default();
        live.push(message("l1", "from the live stream"));
        assert!(!live.restore(vec![message("h1", "from history")]));
        assert_eq!(live.items()[0].id(), Some("l1"));
    }

    #[test]
    fn load_state_transitions_report_only_real_changes() {
        let mut transcript = BackgroundTaskTranscript::default();
        assert_eq!(
            transcript.state(),
            &BackgroundTaskTranscriptState::NotLoaded
        );
        assert!(transcript.set_state(BackgroundTaskTranscriptState::Loading));
        assert!(!transcript.set_state(BackgroundTaskTranscriptState::Loading));
        assert!(transcript.set_state(BackgroundTaskTranscriptState::Ready));
    }

    #[test]
    fn a_failed_load_reports_itself_without_discarding_known_items() {
        let mut transcript = BackgroundTaskTranscript::default();
        transcript.push(message("a", "already seen"));

        BackgroundTaskTranscriptUpdate::state(BackgroundTaskTranscriptState::Unavailable {
            message: "thread/read failed".into(),
        })
        .apply_to(&mut transcript);

        assert_eq!(transcript.items().len(), 1);
        assert!(matches!(
            transcript.state(),
            BackgroundTaskTranscriptState::Unavailable { .. }
        ));
    }

    #[test]
    fn an_update_reports_whether_it_changed_anything() {
        let mut transcript = BackgroundTaskTranscript::default();
        assert!(
            BackgroundTaskTranscriptUpdate::appended(vec![message("a", "one")])
                .apply_to(&mut transcript)
        );
        assert!(
            !BackgroundTaskTranscriptUpdate::appended(Vec::new()).apply_to(&mut transcript),
            "an empty append with an unchanged state is not a repaint"
        );

        let loaded = BackgroundTaskTranscriptUpdate::loaded(vec![message("a", "one")]);
        assert!(
            !loaded.apply_to(&mut transcript),
            "a read matching what is already shown changes nothing"
        );
    }
}

mod restored_child_transcript {
    use crate::background_task::{BackgroundTaskTranscript, BackgroundTaskTranscriptUpdate};
    use crate::chat::Item;

    fn message(id: &str) -> Item {
        Item::AgentMessage {
            id: id.into(),
            text: Some("line".into()),
        }
    }

    #[test]
    fn history_fills_a_child_nothing_was_seen_for() {
        let mut transcript = BackgroundTaskTranscript::default();
        assert!(
            BackgroundTaskTranscriptUpdate::restored(vec![message("h1")]).apply_to(&mut transcript)
        );
        assert_eq!(transcript.items()[0].id(), Some("h1"));
    }

    #[test]
    fn history_never_replaces_content_the_live_stream_produced() {
        let mut transcript = BackgroundTaskTranscript::default();
        BackgroundTaskTranscriptUpdate::appended(vec![message("l1")]).apply_to(&mut transcript);

        BackgroundTaskTranscriptUpdate::restored(vec![message("h1")]).apply_to(&mut transcript);

        assert_eq!(transcript.items().len(), 1);
        assert_eq!(
            transcript.items()[0].id(),
            Some("l1"),
            "the live conversation is newer than the file on disk"
        );
    }
}
