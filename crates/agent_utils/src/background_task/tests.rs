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
