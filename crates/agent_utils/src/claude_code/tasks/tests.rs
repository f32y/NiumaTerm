use serde_json::{Value, json};

use crate::background_task::{BackgroundTaskKey, BackgroundTaskRefs, BackgroundTaskState};
use crate::chat::Item;
use crate::claude_code::tasks::ClaudeTasks;

const SESSION: &str = "sess-1";

fn reducer() -> ClaudeTasks {
    let mut tasks = ClaudeTasks::default();
    tasks.observe(&json!({"type": "system", "subtype": "init", "session_id": SESSION}));
    tasks
}

fn launch(tool_use_id: &str) -> Value {
    json!({
        "type": "assistant",
        "session_id": SESSION,
        "parent_tool_use_id": null,
        "message": {"content": [{
            "type": "tool_use",
            "id": tool_use_id,
            "name": "Task",
            "input": {
                "description": "Review the diff",
                "prompt": "Read the changed files and report issues",
                "subagent_type": "code-reviewer",
            },
        }]},
    })
}

fn tool_result(tool_use_id: &str, is_error: bool) -> Value {
    json!({
        "type": "user",
        "session_id": SESSION,
        "parent_tool_use_id": null,
        "message": {"content": [{
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "is_error": is_error,
            "content": "three issues found",
        }]},
    })
}

fn state_of(tasks: &ClaudeTasks, id: &str) -> Option<BackgroundTaskState> {
    tasks
        .snapshot()?
        .tasks
        .into_iter()
        .find(|task| task.key == BackgroundTaskKey::claude_code(id))
        .map(|task| task.state)
}

#[test]
fn a_task_launch_creates_a_starting_row_before_any_child_activity() {
    let mut tasks = reducer();
    assert!(tasks.observe(&launch("toolu_1")));

    let snapshot = tasks.snapshot().expect("session is known");
    assert_eq!(snapshot.tasks.len(), 1);
    let task = &snapshot.tasks[0];
    assert_eq!(task.state, BackgroundTaskState::Starting);
    assert_eq!(task.display_name.as_deref(), Some("Review the diff"));
    assert_eq!(task.agent_type.as_deref(), Some("code-reviewer"));
    assert_eq!(
        task.objective.as_deref(),
        Some("Read the changed files and report issues")
    );
    assert_eq!(task.parent_session, BackgroundTaskKey::claude_code(SESSION));
    assert_eq!(
        task.refs,
        BackgroundTaskRefs::ClaudeCode {
            task_id: None,
            tool_use_id: Some("toolu_1".into()),
            agent_id: None,
        }
    );
}

#[test]
fn linked_sidechain_activity_advances_the_row_without_entering_the_transcript() {
    let mut tasks = reducer();
    tasks.observe(&launch("toolu_1"));

    assert!(tasks.observe(&json!({
        "type": "assistant",
        "session_id": SESSION,
        "parent_tool_use_id": "toolu_1",
        "message": {"content": [{"type": "text", "text": "reading   src/main.rs"}]},
    })));

    let snapshot = tasks.snapshot().expect("session is known");
    let task = &snapshot.tasks[0];
    assert_eq!(task.state, BackgroundTaskState::Working);
    assert_eq!(task.status.as_deref(), Some("reading src/main.rs"));
    assert_eq!(task.last_preview.as_deref(), Some("reading src/main.rs"));
}

#[test]
fn unlinked_sidechain_activity_never_creates_a_row() {
    let mut tasks = reducer();
    assert!(!tasks.observe(&json!({
        "type": "assistant",
        "session_id": SESSION,
        "parent_tool_use_id": "toolu_unknown",
        "message": {"content": [{"type": "text", "text": "orphan branch"}]},
    })));
    assert!(tasks.snapshot().expect("session is known").tasks.is_empty());
}

#[test]
fn a_matching_result_completes_the_task_and_an_error_result_fails_it() {
    let mut done = reducer();
    done.observe(&launch("toolu_1"));
    assert!(done.observe(&tool_result("toolu_1", false)));
    assert_eq!(state_of(&done, "toolu_1"), Some(BackgroundTaskState::Done));

    let mut failed = reducer();
    failed.observe(&launch("toolu_1"));
    assert!(failed.observe(&tool_result("toolu_1", true)));
    assert_eq!(
        state_of(&failed, "toolu_1"),
        Some(BackgroundTaskState::Failed)
    );
}

#[test]
fn a_result_for_another_tool_leaves_every_task_untouched() {
    let mut tasks = reducer();
    tasks.observe(&launch("toolu_1"));
    let baseline = tasks.snapshot();

    assert!(!tasks.observe(&tool_result("toolu_other", false)));
    assert_eq!(tasks.snapshot(), baseline);
}

#[test]
fn lifecycle_records_map_onto_the_shared_states() {
    // `task_notification` reports only terminal outcomes; a task killed through
    // the CLI's own stop path reports `killed` in a `task_updated` patch and
    // may never emit a notification at all.
    for (record, expected) in [
        (
            json!({"subtype": "task_started", "task_type": "local_agent"}),
            BackgroundTaskState::Working,
        ),
        (
            json!({"subtype": "task_progress", "last_tool_name": "Read"}),
            BackgroundTaskState::Working,
        ),
        (
            json!({"subtype": "task_notification", "status": "completed", "summary": "done"}),
            BackgroundTaskState::Done,
        ),
        (
            json!({"subtype": "task_notification", "status": "failed"}),
            BackgroundTaskState::Failed,
        ),
        (
            json!({"subtype": "task_notification", "status": "stopped"}),
            BackgroundTaskState::Stopped,
        ),
        (
            json!({"subtype": "task_updated", "patch": {"status": "running"}}),
            BackgroundTaskState::Working,
        ),
        (
            json!({"subtype": "task_updated", "patch": {"status": "killed"}}),
            BackgroundTaskState::Stopped,
        ),
        (
            json!({"subtype": "task_updated", "patch": {"status": "completed"}}),
            BackgroundTaskState::Done,
        ),
    ] {
        let mut tasks = reducer();
        tasks.observe(&launch("toolu_1"));
        let mut record = record;
        record["type"] = json!("system");
        record["session_id"] = json!(SESSION);
        record["tool_use_id"] = json!("toolu_1");

        assert!(tasks.observe(&record), "{record}");
        assert_eq!(state_of(&tasks, "toolu_1"), Some(expected), "{record}");
    }
}

#[test]
fn only_agent_work_becomes_a_row() {
    // Background shells, monitors, and workflows travel through the same
    // lifecycle records and must stay out of a child-agent view.
    for task_type in [
        "local_bash",
        "local_workflow",
        "monitor_mcp",
        "monitor_ws",
        "in_process_teammate",
    ] {
        let mut tasks = reducer();
        assert!(!tasks.observe(&json!({
            "type": "system",
            "subtype": "task_started",
            "session_id": SESSION,
            "task_id": format!("{task_type}-1"),
            "task_type": task_type,
            "description": "npm run dev",
        })));
        assert!(tasks.snapshot().expect("session is known").tasks.is_empty());
    }

    // A record with no task type at all is not assumed to be an agent.
    let mut tasks = reducer();
    assert!(!tasks.observe(&json!({
        "type": "system",
        "subtype": "task_started",
        "session_id": SESSION,
        "task_id": "unknown-1",
    })));
    assert!(tasks.snapshot().expect("session is known").tasks.is_empty());

    // It may still enrich a row a parent Task launch already created.
    tasks.observe(&launch("toolu_1"));
    assert!(tasks.observe(&json!({
        "type": "system",
        "subtype": "task_progress",
        "session_id": SESSION,
        "tool_use_id": "toolu_1",
        "last_tool_name": "Grep",
    })));
    let snapshot = tasks.snapshot().expect("session is known");
    assert_eq!(snapshot.tasks[0].status.as_deref(), Some("Grep"));
}

#[test]
fn a_paused_task_shows_needs_input_while_a_parent_approval_does_not() {
    let mut tasks = reducer();
    tasks.observe(&launch("toolu_1"));

    assert!(tasks.observe(&json!({
        "type": "system",
        "subtype": "task_updated",
        "session_id": SESSION,
        "tool_use_id": "toolu_1",
        "patch": {"status": "paused"},
    })));
    assert_eq!(
        state_of(&tasks, "toolu_1"),
        Some(BackgroundTaskState::NeedsInput)
    );
    assert_eq!(
        tasks
            .snapshot()
            .expect("session is known")
            .needs_input_count(),
        1
    );

    // The parent's own approval arrives as a control request, which carries no
    // task association and therefore changes no child row.
    let baseline = tasks.snapshot();
    assert!(!tasks.observe(&json!({
        "type": "control_request",
        "session_id": SESSION,
        "request": {"subtype": "can_use_tool", "tool_name": "Bash"},
    })));
    assert_eq!(tasks.snapshot(), baseline);
}

#[test]
fn identifiers_are_aliased_only_when_one_record_carries_them_together() {
    let mut tasks = reducer();
    tasks.observe(&launch("toolu_1"));

    // A record naming both identifiers ties them to the same row.
    tasks.observe(&json!({
        "type": "system",
        "subtype": "task_started",
        "session_id": SESSION,
        "task_type": "local_agent",
        "task_id": "task_a",
        "tool_use_id": "toolu_1",
    }));
    assert_eq!(
        tasks.snapshot().expect("session is known").tasks.len(),
        1,
        "the aliased record must not create a second row"
    );

    // Later records may use either identifier.
    tasks.observe(&json!({
        "type": "system",
        "subtype": "task_notification",
        "session_id": SESSION,
        "task_id": "task_a",
        "status": "completed",
        "summary": "reviewed 3 files",
    }));
    assert_eq!(state_of(&tasks, "toolu_1"), Some(BackgroundTaskState::Done));

    // An unrelated identifier with no stated relationship stays separate.
    tasks.observe(&json!({
        "type": "system",
        "subtype": "task_started",
        "session_id": SESSION,
        "task_type": "local_agent",
        "task_id": "task_unrelated",
    }));
    assert_eq!(tasks.snapshot().expect("session is known").tasks.len(), 2);
}

#[test]
fn an_explicit_lifecycle_record_resumes_a_finished_task() {
    let mut tasks = reducer();
    tasks.observe(&launch("toolu_1"));
    tasks.observe(&tool_result("toolu_1", false));
    assert_eq!(state_of(&tasks, "toolu_1"), Some(BackgroundTaskState::Done));

    tasks.observe(&json!({
        "type": "system",
        "subtype": "task_started",
        "session_id": SESSION,
        "task_type": "local_agent",
        "tool_use_id": "toolu_1",
    }));
    assert_eq!(
        state_of(&tasks, "toolu_1"),
        Some(BackgroundTaskState::Working)
    );
}

#[test]
fn a_process_boundary_stops_children_the_previous_process_owned() {
    let mut tasks = reducer();
    tasks.observe(&launch("toolu_1"));
    tasks.observe(&json!({
        "type": "system",
        "subtype": "task_started",
        "session_id": SESSION,
        "task_type": "local_agent",
        "tool_use_id": "toolu_1",
    }));
    assert_eq!(
        state_of(&tasks, "toolu_1"),
        Some(BackgroundTaskState::Working)
    );

    // A second `init` is a new CLI process, which cannot still be running the
    // children the previous one owned.
    tasks.observe(&json!({"type": "system", "subtype": "init", "session_id": SESSION}));
    assert_eq!(
        state_of(&tasks, "toolu_1"),
        Some(BackgroundTaskState::Stopped)
    );
}

#[test]
fn a_child_started_in_this_process_survives_its_own_init() {
    let mut tasks = reducer();
    // The launch lands after this process announced itself, so the boundary
    // that created its epoch must not retire it.
    tasks.observe(&launch("toolu_1"));
    tasks.observe(&json!({
        "type": "system",
        "subtype": "task_started",
        "session_id": SESSION,
        "task_type": "local_agent",
        "tool_use_id": "toolu_1",
    }));

    let baseline = tasks.snapshot();
    assert_eq!(
        state_of(&tasks, "toolu_1"),
        Some(BackgroundTaskState::Working)
    );
    assert_eq!(tasks.snapshot(), baseline);
}

#[test]
fn the_live_background_set_never_retires_a_running_child() {
    let mut tasks = reducer();
    tasks.observe(&launch("toolu_1"));

    // A subagent is registered in the foreground and only flips to backgrounded
    // later, so this snapshot legitimately omits a child that is still working.
    assert!(!tasks.observe(&json!({
        "type": "system",
        "subtype": "background_tasks_changed",
        "session_id": SESSION,
        "tasks": [],
    })));
    assert_eq!(
        state_of(&tasks, "toolu_1"),
        Some(BackgroundTaskState::Starting)
    );
}

#[test]
fn a_subagent_stop_hook_lands_only_on_a_child_an_earlier_record_identified() {
    let mut tasks = reducer();
    tasks.observe(&launch("toolu_1"));
    // The hook identifies its child by `agent_id` alone, so it can only match
    // once some earlier record tied that id to this task.
    tasks.observe(&json!({
        "type": "system",
        "subtype": "task_started",
        "session_id": SESSION,
        "task_type": "local_agent",
        "tool_use_id": "toolu_1",
        "agent_id": "agent_7",
    }));

    assert!(tasks.observe(&json!({
        "type": "system",
        "subtype": "hook_response",
        "hook_event": "SubagentStop",
        "session_id": SESSION,
        "agent_id": "agent_7",
    })));
    assert_eq!(state_of(&tasks, "toolu_1"), Some(BackgroundTaskState::Done));

    // An unmatched hook stays ignored rather than being charged to the most
    // recent task.
    let baseline = tasks.snapshot();
    assert!(!tasks.observe(&json!({
        "type": "system",
        "subtype": "hook_response",
        "hook_event": "SubagentStop",
        "session_id": SESSION,
        "agent_id": "agent_unknown",
    })));
    assert_eq!(tasks.snapshot(), baseline);
}

#[test]
fn an_ordinary_user_message_changes_nothing() {
    let mut tasks = reducer();
    tasks.observe(&launch("toolu_1"));
    let baseline = tasks.snapshot();

    assert!(!tasks.observe(&json!({
        "type": "user",
        "session_id": SESSION,
        "parent_tool_use_id": null,
        "message": {"content": [{"type": "text", "text": "keep going"}]},
    })));
    assert_eq!(tasks.snapshot(), baseline);
}

#[test]
fn switching_to_another_session_drops_the_previous_rows() {
    let mut tasks = reducer();
    tasks.observe(&launch("toolu_1"));

    tasks.observe(&json!({"type": "system", "subtype": "init", "session_id": "sess-2"}));
    let snapshot = tasks.snapshot().expect("session is known");
    assert!(snapshot.tasks.is_empty());
    assert_eq!(
        snapshot.parent_session,
        BackgroundTaskKey::claude_code("sess-2")
    );
}

#[test]
fn linked_activity_becomes_the_childs_own_conversation() {
    let mut tasks = reducer();
    tasks.observe(&launch("toolu_1"));
    tasks.take_transcripts();

    tasks.observe(&json!({
        "type": "assistant",
        "session_id": SESSION,
        "parent_tool_use_id": "toolu_1",
        "uuid": "u-1",
        "message": {"content": [
            {"type": "thinking", "id": "th-1", "thinking": "planning the review"},
            {"type": "text", "id": "tx-1", "text": "found three issues"},
            {"type": "tool_use", "id": "tu-1", "name": "Read", "input": {"file_path": "src/lib.rs"}},
        ]},
    }));

    let published = tasks.take_transcripts();
    assert_eq!(published.len(), 1);
    let (key, update) = &published[0];
    assert_eq!(*key, BackgroundTaskKey::claude_code("toolu_1"));
    assert!(
        !update.replace,
        "live activity extends rather than replaces"
    );
    // The same item kinds the parent conversation renders, so a child reads
    // identically rather than through a second presentation.
    assert!(matches!(update.items[0], Item::Reasoning { .. }));
    assert!(matches!(update.items[1], Item::AgentMessage { .. }));
    assert!(matches!(update.items[2], Item::Other { .. }));
}

#[test]
fn linked_user_text_becomes_the_childs_first_instruction() {
    let mut tasks = reducer();
    tasks.observe(&launch("toolu_1"));
    tasks.take_transcripts();

    tasks.observe(&json!({
        "type": "user",
        "session_id": SESSION,
        "parent_tool_use_id": "toolu_1",
        "uuid": "u-0",
        "message": {"content": [
            {"type": "text", "text": "review the parser"},
        ]},
    }));

    let published = tasks.take_transcripts();
    assert_eq!(published.len(), 1);
    assert!(matches!(
        &published[0].1.items[0],
        Item::UserMessage { text: Some(text) } if text == "review the parser"
    ));
}

#[test]
fn a_launch_opens_the_childs_conversation_with_its_instructions() {
    let mut tasks = reducer();
    tasks.observe(&launch("toolu_1"));

    // Claude Code 2.1.2x streams a child's assistant output only, so the
    // launch block is the sole live source of the prompt.
    let published = tasks.take_transcripts();
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].0, BackgroundTaskKey::claude_code("toolu_1"));
    assert!(matches!(
        &published[0].1.items[0],
        Item::UserMessage { text: Some(text) }
            if text == "Read the changed files and report issues"
    ));

    // An older CLI also replays the same prompt as sidechain traffic.
    tasks.observe(&json!({
        "type": "user",
        "session_id": SESSION,
        "parent_tool_use_id": "toolu_1",
        "uuid": "u-0",
        "message": {"content": [
            {"type": "text", "text": "Read the changed files and report issues"},
        ]},
    }));

    assert!(tasks.take_transcripts().is_empty());
}

#[test]
fn a_child_tool_result_completes_the_call_it_answers() {
    let mut tasks = reducer();
    tasks.observe(&launch("toolu_1"));
    tasks.observe(&json!({
        "type": "assistant",
        "session_id": SESSION,
        "parent_tool_use_id": "toolu_1",
        "message": {"content": [
            {"type": "tool_use", "id": "tu-1", "name": "Read", "input": {"file_path": "src/lib.rs"}},
        ]},
    }));
    tasks.take_transcripts();

    tasks.observe(&json!({
        "type": "user",
        "session_id": SESSION,
        "parent_tool_use_id": "toolu_1",
        "message": {"content": [
            {"type": "tool_result", "tool_use_id": "tu-1", "content": "fn main() {}"},
        ]},
    }));

    let published = tasks.take_transcripts();
    assert_eq!(published.len(), 1);
    let items = &published[0].1.items;
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].id(),
        Some("tu-1"),
        "the result completes the started call instead of adding a row"
    );
}

#[test]
fn unlinked_sidechain_activity_publishes_no_conversation() {
    let mut tasks = reducer();
    tasks.observe(&launch("toolu_1"));
    tasks.take_transcripts();

    tasks.observe(&json!({
        "type": "assistant",
        "session_id": SESSION,
        "parent_tool_use_id": "toolu_unknown",
        "message": {"content": [{"type": "text", "id": "t1", "text": "orphan"}]},
    }));

    assert!(tasks.take_transcripts().is_empty());
}

#[test]
fn switching_sessions_drops_pending_child_conversations() {
    let mut tasks = reducer();
    tasks.observe(&launch("toolu_1"));
    tasks.observe(&json!({
        "type": "assistant",
        "session_id": SESSION,
        "parent_tool_use_id": "toolu_1",
        "message": {"content": [{"type": "text", "id": "t1", "text": "work"}]},
    }));

    tasks.observe(&json!({"type": "system", "subtype": "init", "session_id": "sess-2"}));

    assert!(tasks.take_transcripts().is_empty());
}
