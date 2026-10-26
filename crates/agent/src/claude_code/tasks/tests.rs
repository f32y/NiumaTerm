use std::path::PathBuf;
use std::{env, fs, process};

use serde_json::{Value, json};

use crate::background_task::{
    BackgroundTaskKey, BackgroundTaskKind, BackgroundTaskRefs, BackgroundTaskState,
    BackgroundTaskUpdate,
};
use crate::chat::Item;
use crate::claude_code::sessions::RestoredTask;
use crate::claude_code::tasks::shells::{MAX_OUTPUT_BYTES, ShellIndex, shell_items};
use crate::claude_code::tasks::{ClaudeTasks, ShellDetail};

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
fn a_result_for_another_tool_leaves_every_task_untouched() {
    let mut tasks = reducer();

    tasks.observe(&launch("toolu_1"));

    let baseline = tasks.snapshot();

    assert!(!tasks.observe(&tool_result("toolu_other", false)));
    assert_eq!(tasks.snapshot(), baseline);
}

/// `stop_task` names a child by the task id the CLI registered it under. A row
/// that exists only from the parent's tool-use block has not been given one, so
/// it offers no Stop control until a lifecycle record supplies the id, and the
/// tool-use id is never substituted, because it belongs to the parent's tool
/// call, not to the task registry.
#[test]
fn a_child_is_stoppable_once_a_lifecycle_record_names_its_task() {
    let mut tasks = reducer();

    tasks.observe(&launch("toolu_1"));

    let snapshot = tasks.snapshot().expect("session is known");

    assert_eq!(snapshot.tasks[0].state, BackgroundTaskState::Starting);
    assert!(
        !snapshot.tasks[0].can_stop,
        "a row known only by its tool-use id names nothing to stop"
    );

    let key = snapshot.tasks[0].key.clone();

    assert_eq!(tasks.stop_target(&key), None);

    tasks.observe(&json!({
        "type": "system",
        "subtype": "task_started",
        "session_id": SESSION,
        "task_id": "task-9",
        "tool_use_id": "toolu_1",
        "task_type": "local_agent",
    }));

    let snapshot = tasks.snapshot().expect("session is known");

    assert_eq!(snapshot.tasks.len(), 1, "the ids describe one child");
    assert!(snapshot.tasks[0].can_stop);

    // The row keeps its original key, so the lookup has to resolve the alias
    // instead of assuming the key is the task id.
    assert_eq!(tasks.stop_target(&key), Some("task-9"));

    tasks.observe(&json!({
        "type": "system",
        "subtype": "task_notification",
        "session_id": SESSION,
        "task_id": "task-9",
        "status": "stopped",
    }));

    let snapshot = tasks.snapshot().expect("session is known");

    assert!(snapshot.tasks[0].state.is_terminal());
    assert!(
        !snapshot.tasks[0].can_stop,
        "a settled child has nothing left to stop"
    );
    assert_eq!(tasks.stop_target(&key), None);
}

#[test]
fn only_admitted_work_becomes_a_row() {
    // Monitors, workflows, and teammates travel through the same lifecycle
    // records and belong to other views. A shell is registered by every `Bash`
    // call, so one that has not said it is backgrounded is the foreground
    // command the parent transcript already shows.
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

    // The parent's own approval arrives as a control request, which has no
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
fn identifiers_are_aliased_only_when_one_record_holds_them_together() {
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
fn repeated_init_preserves_running_children_and_shells() {
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

    tasks.observe(&bash_launch("toolu_shell", "sleep 20"));

    let mut shell = shell_started(true);

    shell["tool_use_id"] = json!("toolu_shell");

    tasks.observe(&shell);

    tasks.observe(&json!({"type": "system", "subtype": "init", "session_id": SESSION}));

    assert_eq!(
        state_of(&tasks, "toolu_1"),
        Some(BackgroundTaskState::Working)
    );
    assert_eq!(tasks.snapshot().unwrap().active_count(), 2);
}

#[test]
fn an_async_agent_result_keeps_the_child_running_until_its_notification() {
    let mut tasks = reducer();

    tasks.observe(&launch("toolu_1"));

    tasks.observe(&json!({
        "type": "system", "subtype": "task_started", "session_id": SESSION,
        "task_id": "agent-1", "tool_use_id": "toolu_1", "task_type": "local_agent",
    }));

    let mut result = tool_result("toolu_1", false);

    result["tool_use_result"] = json!({
        "isAsync": true, "status": "async_launched", "agentId": "agent-1",
    });

    tasks.observe(&result);
    tasks.observe(&json!({"type": "result", "session_id": SESSION, "subtype": "success"}));

    let snapshot = tasks.snapshot().unwrap();

    assert_eq!(snapshot.active_count(), 1);
    assert_eq!(snapshot.tasks[0].completed_at, None);
    assert_eq!(tasks.stop_target(&snapshot.tasks[0].key), Some("agent-1"));

    tasks.observe(&json!({
        "type": "system", "subtype": "task_notification", "session_id": SESSION,
        "task_id": "agent-1", "status": "completed",
    }));

    assert_eq!(state_of(&tasks, "toolu_1"), Some(BackgroundTaskState::Done));

    tasks.observe(&result);

    assert_eq!(tasks.snapshot().unwrap().active_count(), 0);
}

#[test]
fn restored_shells_keep_their_output_and_do_not_create_duplicate_rows() {
    let mut tasks = reducer();

    let output = scratch_file("restored.output", b"background output\n");
    let starting_sequence = tasks.begin_restoration();

    tasks.finish_restoration(
        Ok(vec![RestoredTask {
            id: "shell-1".into(),
            update: BackgroundTaskUpdate {
                kind: Some(BackgroundTaskKind::Shell),
                state: Some(BackgroundTaskState::Working),
                refs: Some(BackgroundTaskRefs::ClaudeCode {
                    task_id: Some("shell-1".into()),
                    tool_use_id: Some("toolu_shell".into()),
                    agent_id: None,
                }),
                objective: Some("sleep 20".into()),
                display_name: Some("Wait in background".into()),
                ..BackgroundTaskUpdate::default()
            },
            items: Vec::new(),
            output_file: Some(output.to_string_lossy().into_owned()),
        }]),
        starting_sequence,
    );

    let detail = tasks.shell_detail("toolu_shell").unwrap();

    assert_eq!(detail.state, BackgroundTaskState::Stopped);

    let (command, text, _) = command_item(&detail);

    assert_eq!(command, "sleep 20");
    assert_eq!(text.as_deref(), Some("background output\n"));

    tasks.observe(&json!({
        "type":"system", "subtype":"task_notification", "session_id":SESSION,
        "tool_use_id":"toolu_shell", "status":"completed",
    }));

    let snapshot = tasks.snapshot().unwrap();

    assert_eq!(snapshot.tasks.len(), 1);
    assert_eq!(snapshot.tasks[0].state, BackgroundTaskState::Done);

    fs::remove_file(output).unwrap();
}

#[test]
fn restoration_merges_with_a_live_row_first_identified_by_task_id() {
    let mut tasks = reducer();

    let sequence = tasks.begin_restoration();

    tasks.observe(&json!({
        "type": "system",
        "subtype": "task_started",
        "session_id": SESSION,
        "task_id": "agent-1",
        "task_type": "local_agent",
        "tool_use_id": "toolu_1",
    }));

    tasks.finish_restoration(
        Ok(vec![RestoredTask {
            id: "toolu_1".into(),
            update: BackgroundTaskUpdate {
                state: Some(BackgroundTaskState::Done),
                refs: Some(BackgroundTaskRefs::ClaudeCode {
                    task_id: None,
                    tool_use_id: Some("toolu_1".into()),
                    agent_id: None,
                }),
                ..BackgroundTaskUpdate::default()
            },
            items: Vec::new(),
            output_file: None,
        }]),
        sequence,
    );

    let snapshot = tasks.snapshot().unwrap();

    assert_eq!(snapshot.tasks.len(), 1);
    assert_eq!(snapshot.tasks[0].key.id, "agent-1");
    assert_eq!(snapshot.tasks[0].state, BackgroundTaskState::Working);
    assert_eq!(tasks.stop_target(&snapshot.tasks[0].key), Some("agent-1"));
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
fn a_subagent_stop_hook_applies_only_to_a_child_an_earlier_record_identified() {
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

    // An unmatched hook stays ignored instead of being charged to the most
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
        "live activity extends instead of replacing"
    );

    // The same item kinds the parent conversation renders, so a child reads
    // identically instead of through a second presentation.
    assert!(matches!(update.items[0], Item::Reasoning { .. }));
    assert!(matches!(update.items[1], Item::AgentMessage { .. }));
    assert!(matches!(update.items[2], Item::Other { .. }));
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

/// The assistant block a `Bash` call arrives in. The command lives only here;
/// every later record names the shell by description.
fn bash_launch(tool_use_id: &str, command: &str) -> Value {
    json!({
        "type": "assistant",
        "session_id": SESSION,
        "parent_tool_use_id": null,
        "message": {"content": [{
            "type": "tool_use",
            "id": tool_use_id,
            "name": "Bash",
            "input": {
                "command": command,
                "description": "Build the app",
                "run_in_background": true,
            },
        }]},
    })
}

fn shell_started(is_backgrounded: bool) -> Value {
    json!({
        "type": "system",
        "subtype": "task_started",
        "session_id": SESSION,
        "task_id": "b8vo1ylgc",
        "tool_use_id": "toolu_1",
        "description": "Build the app",
        "task_type": "local_bash",
        "is_backgrounded": is_backgrounded,
    })
}

fn live_shell_set() -> Value {
    json!({
        "type": "system",
        "subtype": "background_tasks_changed",
        "session_id": SESSION,
        "tasks": [{
            "task_id": "b8vo1ylgc",
            "task_type": "local_bash",
            "description": "Build the app",
        }],
    })
}

#[test]
fn a_backgrounded_command_becomes_a_stoppable_shell_row() {
    let mut tasks = reducer();

    tasks.observe(&bash_launch("toolu_1", "cargo build"));

    assert!(tasks.observe(&shell_started(true)));

    let snapshot = tasks.snapshot().expect("session is known");

    assert_eq!(snapshot.tasks.len(), 1);

    let task = &snapshot.tasks[0];

    assert_eq!(task.key, BackgroundTaskKey::claude_code("b8vo1ylgc"));
    assert_eq!(task.kind, BackgroundTaskKind::Shell);
    assert_eq!(task.state, BackgroundTaskState::Working);
    assert_eq!(task.display_name.as_deref(), Some("Build the app"));

    // The command is what the row is about, so it reads as the row detail.
    assert_eq!(task.objective.as_deref(), Some("cargo build"));
    assert!(task.can_stop);
}

#[test]
fn a_foreground_command_becomes_a_row_only_once_it_is_backgrounded() {
    let mut tasks = reducer();

    tasks.observe(&bash_launch("toolu_1", "cargo build"));

    // Every `Bash` call registers a task; a foreground one is already visible
    // as the tool row that started it.
    assert!(!tasks.observe(&shell_started(false)));
    assert!(tasks.snapshot().expect("session is known").tasks.is_empty());

    // The live set is the only record stating that a command already under way
    // has moved to the background.
    assert!(tasks.observe(&live_shell_set()));

    let snapshot = tasks.snapshot().expect("session is known");
    let task = &snapshot.tasks[0];

    assert_eq!(task.kind, BackgroundTaskKind::Shell);
    assert_eq!(task.state, BackgroundTaskState::Working);

    // The rejected `task_started` still supplied what the live set omits.
    assert_eq!(task.objective.as_deref(), Some("cargo build"));
    assert_eq!(
        task.refs,
        BackgroundTaskRefs::ClaudeCode {
            task_id: Some("b8vo1ylgc".into()),
            tool_use_id: Some("toolu_1".into()),
            agent_id: None,
        }
    );
}

#[test]
fn a_backgrounded_commands_tool_result_is_not_its_outcome() {
    let mut tasks = reducer();

    tasks.observe(&bash_launch("toolu_1", "cargo build"));

    tasks.observe(&shell_started(true));

    // The `Bash` call is answered the moment the command is handed off, with
    // the id it was backgrounded under instead of with what it did.
    tasks.observe(&json!({
        "type": "user",
        "session_id": SESSION,
        "parent_tool_use_id": null,
        "message": {"content": [{
            "type": "tool_result",
            "tool_use_id": "toolu_1",
            "content": "Command running in background with ID: b8vo1ylgc.",
        }]},
    }));

    assert_eq!(
        state_of(&tasks, "b8vo1ylgc"),
        Some(BackgroundTaskState::Working)
    );
}

#[test]
fn the_handoff_result_names_the_output_file_a_running_command_writes_to() {
    let mut tasks = reducer();

    tasks.observe(&bash_launch("toolu_1", "cargo build"));

    tasks.observe(&shell_started(true));

    tasks.observe(&json!({
        "type": "user",
        "session_id": SESSION,
        "parent_tool_use_id": null,
        "message": {"content": [{
            "type": "tool_result",
            "tool_use_id": "toolu_1",
            // A real handoff result is longer than the preview bound a row's
            // status line is condensed to, and a directory containing spaces
            // still has to survive the read.
            "content": "Command running in background with ID: b8vo1ylgc. \
                Output is being written to: \
                C:\\Users\\Ada Lovelace\\AppData\\Local\\Temp\\claude\\C--Workspace-NiumaTerm\\\
                217943eb-a012-4714-9220-ce76bcc960a2\\tasks\\b8vo1ylgc.output. \
                You will be notified when it completes. To check interim output, \
                use Read on that file path.",
        }]},
    }));

    let detail = tasks.shell_detail("b8vo1ylgc").expect("row is a shell");

    assert_eq!(
        detail.output_file.as_deref(),
        Some(
            "C:\\Users\\Ada Lovelace\\AppData\\Local\\Temp\\claude\\C--Workspace-NiumaTerm\\\
             217943eb-a012-4714-9220-ce76bcc960a2\\tasks\\b8vo1ylgc.output"
        )
    );

    // The command is still running, so the card it renders says so.
    assert_eq!(detail.state, BackgroundTaskState::Working);
}

#[test]
fn a_completion_notification_settles_a_shell_and_names_its_output_file() {
    let mut tasks = reducer();

    tasks.observe(&bash_launch("toolu_1", "cargo build"));

    tasks.observe(&shell_started(true));

    assert!(tasks.observe(&json!({
        "type": "system",
        "subtype": "task_notification",
        "session_id": SESSION,
        "task_id": "b8vo1ylgc",
        "tool_use_id": "toolu_1",
        "status": "completed",
        "output_file": "/tmp/b8vo1ylgc.output",
        "summary": "Background command \"Build the app\" completed (exit code 0)",
    })));

    assert_eq!(
        state_of(&tasks, "b8vo1ylgc"),
        Some(BackgroundTaskState::Done)
    );

    let detail = tasks.shell_detail("b8vo1ylgc").expect("row is a shell");

    assert_eq!(detail.id, "b8vo1ylgc");
    assert_eq!(detail.command.as_deref(), Some("cargo build"));
    assert_eq!(detail.output_file.as_deref(), Some("/tmp/b8vo1ylgc.output"));
    assert_eq!(detail.state, BackgroundTaskState::Done);

    // The tool-use id names the same row, so opening it from either identifier
    // reads the same command.
    assert!(tasks.shell_detail("toolu_1").is_some());
}

#[test]
fn the_live_set_does_not_revive_a_shell_that_already_reported_its_outcome() {
    let mut tasks = reducer();

    tasks.observe(&bash_launch("toolu_1", "cargo build"));

    tasks.observe(&shell_started(true));

    tasks.observe(&json!({
        "type": "system",
        "subtype": "task_notification",
        "session_id": SESSION,
        "task_id": "b8vo1ylgc",
        "status": "completed",
    }));

    // The CLI publishes the live set before the terminal record, so a snapshot
    // can still list a command that has just finished.
    tasks.observe(&live_shell_set());

    assert_eq!(
        state_of(&tasks, "b8vo1ylgc"),
        Some(BackgroundTaskState::Done)
    );
}

fn scratch_file(name: &str, contents: &[u8]) -> PathBuf {
    let dir = env::temp_dir().join(format!("nmt-shell-output-{}", process::id()));

    fs::create_dir_all(&dir).expect("scratch directory is writable");

    let path = dir.join(name);

    fs::write(&path, contents).expect("scratch file is writable");

    path
}

fn detail(output_file: Option<String>, state: BackgroundTaskState) -> ShellDetail {
    ShellDetail {
        id: "b8vo1ylgc".to_string(),
        command: Some("cargo build".to_string()),
        description: Some("Build the app".to_string()),
        output_file,
        state,
    }
}

fn command_item(detail: &ShellDetail) -> (String, Option<String>, Option<String>) {
    match shell_items(detail).remove(0) {
        Item::CommandExecution {
            command,
            aggregated_output,
            status,
            ..
        } => (command, aggregated_output, status),
        other => panic!("a shell renders as a command card, got {other:?}"),
    }
}

#[test]
fn an_output_file_longer_than_the_bound_is_shown_from_its_end() {
    let overflow = MAX_OUTPUT_BYTES as usize + 1024;

    let mut contents = vec![b'a'; overflow];

    contents.extend_from_slice(b"final line\n");

    let path = scratch_file("long.output", &contents);

    let (_, output, _) = command_item(&detail(
        Some(path.to_string_lossy().into_owned()),
        BackgroundTaskState::Working,
    ));

    let output = output.expect("the tail is readable");

    assert!(output.ends_with("final line\n"));
    assert!(output.len() <= MAX_OUTPUT_BYTES as usize);
}

/// A shell the background snapshot keeps re-listing takes one slot, so it
/// cannot push another shell's command and description out of the table.
#[test]
fn a_relisted_shell_does_not_evict_another_shells_metadata() {
    let mut shells = ShellIndex::default();

    shells.remember_shell(&json!({"task_id": "first", "description": "Build"}));

    for _ in 0..1_000 {
        shells.reserve_shell_meta("second");
    }

    assert_eq!(
        shells
            .meta("first")
            .and_then(|meta| meta.description.as_deref()),
        Some("Build")
    );
}
