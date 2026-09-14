use std::fs::{self, OpenOptions};
use std::io::Write;

use serde_json::{Value, json};
use tempfile::tempdir;

use crate::claude_code::sessions::progress::ProgressReader;
use crate::claude_code::sessions::progress::tracker::ProgressTracker;
use crate::progress::TaskStatus;

fn call(tracker: &mut ProgressTracker, name: &str, input: Value, output: Value, failed: bool) {
    tracker.observe(&json!({"type": "assistant", "message": {"content": [
        {"type": "tool_use", "id": "call", "name": name, "input": input}
    ]}}));

    tracker.observe(&json!({"type": "user", "toolUseResult": output, "message": {"content": [
        {"type": "tool_result", "tool_use_id": "call", "is_error": failed, "content": "acknowledged"}
    ]}}));
}

#[test]
fn task_tools_keep_details_and_use_assigned_ids_until_deletion() {
    let mut tracker = ProgressTracker::default();

    call(
        &mut tracker,
        "TaskCreate",
        json!({"subject": "Build", "description": "Build both targets"}),
        json!({"task": {"id": "7", "subject": "Build"}}),
        false,
    );

    call(
        &mut tracker,
        "TaskUpdate",
        json!({"taskId": "7", "status": "in_progress", "owner": "Alice", "addBlockedBy": ["3"]}),
        json!({"success": true}),
        false,
    );

    call(
        &mut tracker,
        "TaskList",
        json!({}),
        json!({"tasks": [{"id": "7", "subject": "Build", "status": "in_progress"}]}),
        false,
    );

    let task = &tracker.snapshot.tasks.items[0];

    assert_eq!(task.id, "7");
    assert_eq!(task.description.as_deref(), Some("Build both targets"));
    assert_eq!(task.owner.as_deref(), Some("Alice"));
    assert_eq!(task.blocked_by, ["3"]);
    assert_eq!(task.status, TaskStatus::InProgress);

    call(
        &mut tracker,
        "TaskUpdate",
        json!({"taskId": "7", "status": "completed"}),
        json!({}),
        true,
    );

    assert_eq!(tracker.snapshot.tasks.tally(), Some((0, 1)));

    call(
        &mut tracker,
        "TaskUpdate",
        json!({"taskId": "7", "status": "completed"}),
        json!({}),
        false,
    );

    assert_eq!(tracker.snapshot.tasks.tally(), Some((1, 1)));

    call(
        &mut tracker,
        "TaskUpdate",
        json!({"taskId": "7", "status": "deleted"}),
        json!({}),
        false,
    );

    assert!(tracker.snapshot.tasks.items.is_empty());
}

#[test]
fn whole_list_updates_clear_old_tasks_and_ignore_child_records() {
    let mut tracker = ProgressTracker::default();

    call(
        &mut tracker,
        "TodoWrite",
        json!({"todos": [{"content": "Verify", "status": "in_progress"}]}),
        json!({}),
        false,
    );

    assert_eq!(
        tracker.snapshot.tasks.items[0].status,
        TaskStatus::InProgress
    );

    tracker.observe(&json!({"type": "attachment", "isSidechain": true, "attachment": {"type": "goal_status", "condition": "Child", "met": false}}));

    assert!(tracker.snapshot.goal.is_none());

    call(
        &mut tracker,
        "TodoWrite",
        json!({"todos": []}),
        json!({}),
        true,
    );

    assert_eq!(tracker.snapshot.tasks.tally(), Some((0, 1)));

    call(
        &mut tracker,
        "TodoWrite",
        json!({"todos": []}),
        json!({}),
        false,
    );

    assert!(tracker.snapshot.tasks.items.is_empty());
}

#[test]
fn goal_attachments_distinguish_creation_evaluation_completion_and_clear() {
    let mut tracker = ProgressTracker::default();

    for (attachment, phase, rounds) in [
        (
            json!({"sentinel": true, "met": false, "condition": "Verify output"}),
            "active",
            0,
        ),
        (
            json!({"met": false, "condition": "Verify output", "reason": "One target remains"}),
            "active",
            1,
        ),
        (
            json!({"met": true, "condition": "Verify output", "reason": "Both targets passed", "iterations": 2, "tokens": 4200, "durationMs": 9500}),
            "complete",
            2,
        ),
    ] {
        let mut attachment = attachment;

        attachment["type"] = json!("goal_status");

        tracker.observe(&json!({"type": "attachment", "attachment": attachment}));

        let goal = tracker.snapshot.goal.as_ref().unwrap();

        assert_eq!(goal.phase, phase);
        assert_eq!(goal.rounds_started, rounds);
    }

    let goal = tracker.snapshot.goal.as_ref().unwrap();

    assert_eq!(goal.reason.as_deref(), Some("Both targets passed"));
    assert_eq!(goal.tokens_used, Some(4200));
    assert_eq!(goal.elapsed_seconds, Some(9));

    tracker.observe(&json!({"type": "attachment", "attachment": {"type": "goal_status", "sentinel": true, "met": true, "condition": "Verify output"}}));

    assert!(tracker.snapshot.goal.is_none());
}

#[test]
fn incremental_reader_waits_for_complete_records_and_recovers_after_truncation() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let first = json!({"type": "attachment", "attachment": {"type": "goal_status", "condition": "Build", "sentinel": true, "met": false}}).to_string();

    fs::write(&path, format!("{first}\n")).unwrap();

    let mut reader = ProgressReader::default();

    assert_eq!(
        reader.read(&path).unwrap().goal.as_ref().unwrap().objective,
        "Build"
    );

    let second = json!({"type": "attachment", "attachment": {"type": "goal_status", "condition": "Build", "met": false, "reason": "Still running"}}).to_string();
    let split = second.len() / 2;

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();

    file.write_all(&second.as_bytes()[..split]).unwrap();

    assert_eq!(
        reader
            .read(&path)
            .unwrap()
            .goal
            .as_ref()
            .unwrap()
            .rounds_started,
        0
    );

    writeln!(file, "{}", &second[split..]).unwrap();

    assert_eq!(
        reader
            .read(&path)
            .unwrap()
            .goal
            .as_ref()
            .unwrap()
            .rounds_started,
        1
    );
    assert_eq!(
        reader
            .read(&path)
            .unwrap()
            .goal
            .as_ref()
            .unwrap()
            .rounds_started,
        1
    );

    fs::write(&path, "{}\n").unwrap();

    assert!(reader.read(&path).unwrap().goal.is_none());
}
