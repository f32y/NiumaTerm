use serde_json::{Value, json};

use crate::background_task::{
    BackgroundTaskDiscoveryState, BackgroundTaskKey, BackgroundTaskRefs, BackgroundTaskState,
};
use crate::chat::Item;
use crate::codex::app_server::THREAD_SCOPED_NOTIFICATIONS;
use crate::codex::app_server::background_tasks::{
    CodexTasks, MAX_PENDING_THREADS, ThreadScope, notification_thread_id,
};

const ROOT: &str = "thr_parent";

fn rooted() -> CodexTasks {
    let mut tasks = CodexTasks::default();
    assert!(tasks.set_root(ROOT));
    assert!(!tasks.set_root(ROOT), "the same root does not reload");
    tasks
}

/// A completed `spawnAgent` call, which is the item that first names the child
/// and reports its initial state.
fn spawn_item(child: &str) -> Value {
    spawn_item_with_status(child, "running")
}

fn spawn_item_with_status(child: &str, status: &str) -> Value {
    json!({
        "type": "collabAgentToolCall",
        "id": "item-1",
        "tool": "spawnAgent",
        "status": "completed",
        "senderThreadId": ROOT,
        "receiverThreadIds": [child],
        "prompt": "review the diff",
        "model": "gpt-5-codex",
        "agentsStates": {child: {"status": status, "message": null}},
    })
}

fn agent_status_item(child: &str, status: &str, message: Option<&str>) -> Value {
    json!({
        "type": "collabAgentToolCall",
        "id": "item-2",
        "tool": "wait",
        "status": "completed",
        "senderThreadId": ROOT,
        "receiverThreadIds": [child],
        "agentsStates": {child: {"status": status, "message": message}},
    })
}

fn state_of(tasks: &CodexTasks, child: &str) -> Option<BackgroundTaskState> {
    tasks
        .snapshot()?
        .tasks
        .into_iter()
        .find(|task| task.key == BackgroundTaskKey::codex(child))
        .map(|task| task.state)
}

#[test]
fn a_spawn_item_confirms_the_child_and_applies_its_held_update() {
    let mut tasks = rooted();

    // The child's first turn can arrive before the parent's spawn item.
    assert_eq!(tasks.scope(Some("thr_child")), ThreadScope::Unrelated);
    tasks.hold_unrelated_notification("thr_child", "turn/started", &json!({}));
    assert!(
        tasks.snapshot().expect("registry exists").tasks.is_empty(),
        "an unconfirmed thread never creates a row"
    );

    assert!(tasks.observe_parent_item(&spawn_item("thr_child")));
    assert_eq!(tasks.scope(Some("thr_child")), ThreadScope::Descendant);

    let snapshot = tasks.snapshot().expect("registry exists");
    assert_eq!(snapshot.tasks.len(), 1);
    let child = &snapshot.tasks[0];
    assert_eq!(child.state, BackgroundTaskState::Working);
    assert_eq!(child.objective.as_deref(), Some("review the diff"));
    assert_eq!(child.model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(child.parent_session, BackgroundTaskKey::codex(ROOT));
    assert_eq!(
        child.refs,
        BackgroundTaskRefs::Codex {
            thread_id: "thr_child".into(),
            parent_thread_id: Some(ROOT.into()),
        }
    );
}

#[test]
fn unrelated_threads_are_evicted_instead_of_accumulating() {
    let mut tasks = rooted();
    for index in 0..MAX_PENDING_THREADS + 5 {
        tasks.hold_unrelated_notification(
            &format!("thr_other_{index}"),
            "turn/started",
            &json!({}),
        );
    }

    // The oldest candidates were dropped, so confirming one later adds nothing.
    assert!(tasks.observe_parent_item(&spawn_item("thr_other_0")));
    assert_eq!(
        state_of(&tasks, "thr_other_0"),
        Some(BackgroundTaskState::Working),
        "a confirmed thread still gets its row from the spawn item itself"
    );

    let snapshot = tasks.snapshot().expect("registry exists");
    assert_eq!(
        snapshot.tasks.len(),
        1,
        "held candidates never become rows on their own"
    );
}

#[test]
fn a_child_turn_completion_reports_a_terminal_state_and_an_explicit_resume_reopens_it() {
    let mut tasks = rooted();
    tasks.observe_parent_item(&spawn_item("thr_child"));

    assert!(tasks.apply_descendant_notification(
        "thr_child",
        "turn/completed",
        &json!({"turn": {"status": "completed"}}),
    ));
    assert_eq!(
        state_of(&tasks, "thr_child"),
        Some(BackgroundTaskState::Done)
    );

    // Codex can hand more work to a finished child, so terminal is not final.
    assert!(tasks.apply_descendant_notification("thr_child", "turn/started", &json!({})));
    assert_eq!(
        state_of(&tasks, "thr_child"),
        Some(BackgroundTaskState::Working)
    );
}

#[test]
fn live_events_apply_in_arrival_order() {
    let mut tasks = rooted();
    tasks.observe_parent_item(&spawn_item("thr_child"));

    // A loaded child waiting on the user reads as Needs Input.
    tasks.apply_descendant_notification(
        "thr_child",
        "thread/status/changed",
        &json!({"status": {"type": "active", "activeFlags": ["waitingOnUserInput"]}}),
    );
    assert_eq!(
        state_of(&tasks, "thr_child"),
        Some(BackgroundTaskState::NeedsInput)
    );
    assert_eq!(
        tasks
            .snapshot()
            .expect("registry exists")
            .needs_input_count(),
        1
    );

    tasks.apply_descendant_notification(
        "thr_child",
        "thread/status/changed",
        &json!({"status": {"type": "active", "activeFlags": []}}),
    );
    assert_eq!(
        state_of(&tasks, "thr_child"),
        Some(BackgroundTaskState::Working)
    );

    // `idle` only means the thread stopped running; it is not an outcome, so
    // the known lifecycle stays put until the parent reports one.
    tasks.apply_descendant_notification(
        "thr_child",
        "thread/status/changed",
        &json!({"status": {"type": "idle"}}),
    );
    assert_eq!(
        state_of(&tasks, "thr_child"),
        Some(BackgroundTaskState::Working)
    );

    tasks.apply_descendant_notification(
        "thr_child",
        "thread/status/changed",
        &json!({"status": {"type": "systemError"}}),
    );
    assert_eq!(
        state_of(&tasks, "thr_child"),
        Some(BackgroundTaskState::Failed)
    );
}

#[test]
fn codex_states_map_onto_the_shared_lifecycle() {
    for (reported, expected) in [
        ("pendingInit", BackgroundTaskState::Starting),
        ("running", BackgroundTaskState::Working),
        ("completed", BackgroundTaskState::Done),
        ("interrupted", BackgroundTaskState::Interrupted),
        ("shutdown", BackgroundTaskState::Stopped),
        ("errored", BackgroundTaskState::Failed),
        ("notFound", BackgroundTaskState::Failed),
    ] {
        let mut tasks = rooted();
        tasks.observe_parent_item(&spawn_item_with_status("thr_child", reported));
        assert_eq!(state_of(&tasks, "thr_child"), Some(expected), "{reported}");
    }

    // The reported message becomes the row's status line.
    let mut tasks = rooted();
    tasks.observe_parent_item(&spawn_item("thr_child"));
    tasks.observe_parent_item(&agent_status_item(
        "thr_child",
        "errored",
        Some("sandbox denied"),
    ));
    let snapshot = tasks.snapshot().expect("registry exists");
    assert_eq!(snapshot.tasks[0].state, BackgroundTaskState::Failed);
    assert_eq!(snapshot.tasks[0].status.as_deref(), Some("sandbox denied"));

    // A child system error is a failure even without a collaboration item.
    tasks.apply_descendant_notification(
        "thr_child",
        "error",
        &json!({"error": {"message": "child crashed"}}),
    );
    assert_eq!(
        state_of(&tasks, "thr_child"),
        Some(BackgroundTaskState::Failed)
    );
}

#[test]
fn subagent_activity_moves_a_known_child_without_naming_a_new_one() {
    let mut tasks = rooted();
    tasks.observe_parent_item(&spawn_item_with_status("thr_child", "pendingInit"));

    assert!(tasks.observe_parent_item(&json!({
        "type": "subAgentActivity",
        "id": "act-1",
        "kind": "started",
        "agentThreadId": "thr_child",
        "agentPath": "reviewer",
    })));
    assert_eq!(
        state_of(&tasks, "thr_child"),
        Some(BackgroundTaskState::Working)
    );

    // `interacted` reports that the parent sent input, not that the child's own
    // state moved.
    tasks.observe_parent_item(&json!({
        "type": "subAgentActivity",
        "id": "act-2",
        "kind": "interacted",
        "agentThreadId": "thr_child",
        "agentPath": "reviewer",
    }));
    assert_eq!(
        state_of(&tasks, "thr_child"),
        Some(BackgroundTaskState::Working)
    );

    tasks.observe_parent_item(&json!({
        "type": "subAgentActivity",
        "id": "act-3",
        "kind": "interrupted",
        "agentThreadId": "thr_child",
        "agentPath": "reviewer",
    }));
    assert_eq!(
        state_of(&tasks, "thr_child"),
        Some(BackgroundTaskState::Interrupted)
    );
}

fn can_stop(tasks: &CodexTasks, child: &str) -> bool {
    tasks
        .snapshot()
        .expect("registry exists")
        .tasks
        .into_iter()
        .find(|task| task.key == BackgroundTaskKey::codex(child))
        .expect("child row exists")
        .can_stop
}

/// `turn/interrupt` refuses a turn id that is not the thread's active one, so a
/// child is stoppable exactly while its current turn is known. The row's own
/// lifecycle state is not that signal: a child can read as Working from a
/// collaboration item before any turn of its own has been announced.
#[test]
fn a_child_is_stoppable_only_while_its_active_turn_is_known() {
    let mut tasks = rooted();
    tasks.observe_parent_item(&spawn_item("thr_child"));
    assert_eq!(
        state_of(&tasks, "thr_child"),
        Some(BackgroundTaskState::Working)
    );
    assert!(
        !can_stop(&tasks, "thr_child"),
        "a Working row with no announced turn has nothing to name"
    );
    assert!(tasks.interrupt_request(1, "thr_child").is_none());

    // The turn arriving is a change even though the state does not move, or the
    // panel would keep rendering a row it could now stop.
    assert!(tasks.apply_descendant_notification(
        "thr_child",
        "turn/started",
        &json!({"threadId": "thr_child", "turn": {"id": "turn-1", "status": "inProgress"}}),
    ));
    assert!(can_stop(&tasks, "thr_child"));

    let request = tasks
        .interrupt_request(7, "thr_child")
        .expect("a known turn can be interrupted");
    assert_eq!(request["id"], 7);
    assert_eq!(request["method"], "turn/interrupt");
    assert_eq!(request["params"]["threadId"], "thr_child");
    assert_eq!(request["params"]["turnId"], "turn-1");

    // A child waiting on an approval is still mid-turn, so Stop still applies.
    assert!(tasks.apply_descendant_notification(
        "thr_child",
        "thread/status/changed",
        &json!({"status": {"type": "active", "activeFlags": ["waitingOnApproval"]}}),
    ));
    assert_eq!(
        state_of(&tasks, "thr_child"),
        Some(BackgroundTaskState::NeedsInput)
    );
    assert!(can_stop(&tasks, "thr_child"));

    assert!(tasks.apply_descendant_notification(
        "thr_child",
        "turn/completed",
        &json!({"turn": {"status": "interrupted"}}),
    ));
    assert_eq!(
        state_of(&tasks, "thr_child"),
        Some(BackgroundTaskState::Interrupted)
    );
    assert!(!can_stop(&tasks, "thr_child"));
    assert!(tasks.interrupt_request(8, "thr_child").is_none());
}

/// Going idle ends the turn without being an outcome of its own: the row keeps
/// the lifecycle it had, but there is no longer a turn to interrupt.
#[test]
fn an_idle_child_keeps_its_state_and_loses_its_stop_control() {
    let mut tasks = rooted();
    tasks.observe_parent_item(&spawn_item("thr_child"));
    tasks.apply_descendant_notification(
        "thr_child",
        "turn/started",
        &json!({"threadId": "thr_child", "turn": {"id": "turn-1", "status": "inProgress"}}),
    );
    assert!(can_stop(&tasks, "thr_child"));

    assert!(
        tasks.apply_descendant_notification(
            "thr_child",
            "thread/status/changed",
            &json!({"status": {"type": "idle"}}),
        ),
        "losing the turn is reported even though the status maps to no state"
    );
    assert_eq!(
        state_of(&tasks, "thr_child"),
        Some(BackgroundTaskState::Working)
    );
    assert!(!can_stop(&tasks, "thr_child"));
}

#[test]
fn child_transcript_items_only_update_the_row_preview() {
    let mut tasks = rooted();
    tasks.observe_parent_item(&spawn_item("thr_child"));

    assert!(tasks.apply_descendant_notification(
        "thr_child",
        "item/completed",
        &json!({"item": {"type": "agentMessage", "id": "i1", "text": "found  three   issues"}}),
    ));

    let snapshot = tasks.snapshot().expect("registry exists");
    let child = &snapshot.tasks[0];
    assert_eq!(child.last_preview.as_deref(), Some("found three issues"));
    assert_eq!(
        child.state,
        BackgroundTaskState::Working,
        "a transcript item never changes lifecycle state"
    );
}

#[test]
fn every_parent_mutating_notification_is_routed_by_thread_id() {
    // These are the notifications that change the parent's turn identity,
    // running state, approval state, or transcript. Any of them reaching the
    // parent from a descendant would corrupt the conversation, so each must be
    // classified before parent handling runs.
    for method in [
        "turn/started",
        "turn/completed",
        "item/started",
        "item/completed",
        "item/agentMessage/delta",
        "thread/status/changed",
        "thread/tokenUsage/updated",
        "error",
    ] {
        assert!(
            THREAD_SCOPED_NOTIFICATIONS.contains(&method),
            "{method} bypasses thread routing"
        );
    }

    let tasks = rooted();
    assert_eq!(tasks.scope(Some(ROOT)), ThreadScope::Parent);
    assert_eq!(tasks.scope(None), ThreadScope::Unscoped);
    assert_eq!(
        CodexTasks::default().scope(Some(ROOT)),
        ThreadScope::Unscoped,
        "before a root is known nothing can be routed away from the parent"
    );
}

#[test]
fn notification_thread_ids_are_read_from_every_known_location() {
    assert_eq!(notification_thread_id(&json!({"threadId": "a"})), Some("a"));
    assert_eq!(
        notification_thread_id(&json!({"thread": {"id": "b"}})),
        Some("b")
    );
    assert_eq!(notification_thread_id(&json!({})), None);
}

#[test]
fn descendant_requests_page_through_subagent_spawns() {
    let mut tasks = rooted();
    let request = tasks.descendant_request(7, None).expect("root is known");
    assert_eq!(request["method"], "thread/list");
    assert_eq!(request["params"]["ancestorThreadId"], ROOT);
    assert_eq!(
        request["params"]["sourceKinds"],
        json!(["subAgentThreadSpawn"])
    );
    // An empty provider list opts out of the provider filter entirely.
    assert_eq!(request["params"]["modelProviders"], json!([]));
    assert_eq!(request["params"]["useStateDbOnly"], json!(true));
    assert!(request["params"]["cursor"].is_null());
    assert!(tasks.query_in_flight());
    assert!(matches!(
        tasks.snapshot().expect("registry exists").discovery,
        BackgroundTaskDiscoveryState::Loading
    ));

    let (_, next_cursor) = tasks.apply_descendants(
        7,
        &json!({
            "data": [{
                "id": "thr_a",
                "parentThreadId": ROOT,
                "status": {"type": "idle"},
                "preview": "review the diff",
                "agentNickname": "swift-otter",
                "agentRole": "reviewer",
                "createdAt": 1000,
                "recencyAt": 1200,
            }],
            "nextCursor": "page-2",
        }),
    );
    assert_eq!(next_cursor.as_deref(), Some("page-2"));

    let request = tasks
        .descendant_request(8, next_cursor.as_deref())
        .expect("root is known");
    assert_eq!(request["params"]["cursor"], "page-2");

    let (_, next_cursor) = tasks.apply_descendants(
        8,
        &json!({"data": [{
            "id": "thr_b",
            "parentThreadId": "thr_a",
            "status": {"type": "active", "activeFlags": []},
        }]}),
    );
    assert!(next_cursor.is_none());
    assert!(!tasks.query_in_flight());

    let snapshot = tasks.snapshot().expect("registry exists");
    assert_eq!(snapshot.tasks.len(), 2);
    assert!(matches!(
        snapshot.discovery,
        BackgroundTaskDiscoveryState::Ready
    ));
    // A listed thread that is no longer loaded reads as ended, so a resumed
    // parent shows its past children under Finished.
    let restored = snapshot
        .tasks
        .iter()
        .find(|task| task.key.id == "thr_a")
        .expect("restored row exists");
    assert_eq!(restored.state, BackgroundTaskState::Stopped);
    assert_eq!(restored.display_name.as_deref(), Some("swift-otter"));
    assert_eq!(restored.agent_type.as_deref(), Some("reviewer"));
    assert_eq!(restored.objective.as_deref(), Some("review the diff"));
    assert!(restored.started_at.is_some());
    assert!(restored.completed_at.is_some());
    // A nested descendant keeps its immediate parent and its depth below root.
    let nested = snapshot
        .tasks
        .iter()
        .find(|task| task.key.id == "thr_b")
        .expect("nested row exists");
    assert_eq!(
        nested.refs,
        BackgroundTaskRefs::Codex {
            thread_id: "thr_b".into(),
            parent_thread_id: Some("thr_a".into()),
        }
    );
    assert_eq!(nested.depth, Some(2));
}

#[test]
fn rows_outside_the_selected_root_and_cycles_are_rejected() {
    let mut tasks = rooted();
    tasks.descendant_request(7, None);
    tasks.apply_descendants(
        7,
        &json!({
            "data": [
                {"id": "thr_elsewhere", "parentThreadId": "thr_other_root"},
                {"id": ROOT, "parentThreadId": ROOT},
                {"id": "thr_cycle_a", "parentThreadId": "thr_cycle_b"},
            ]
        }),
    );

    assert!(
        tasks.snapshot().expect("registry exists").tasks.is_empty(),
        "only threads that chain back to the selected root become rows"
    );
}

#[test]
fn a_delayed_query_response_cannot_replace_a_newer_live_state() {
    let mut tasks = rooted();
    tasks.observe_parent_item(&spawn_item("thr_child"));
    tasks.descendant_request(7, None);

    // The child finishes live while the query is still in flight.
    tasks.apply_descendant_notification(
        "thr_child",
        "turn/completed",
        &json!({"turn": {"status": "completed"}}),
    );

    tasks.apply_descendants(
        7,
        &json!({
            "data": [{
                "id": "thr_child",
                "parentThreadId": ROOT,
                "status": {"type": "active", "activeFlags": []},
                "agentNickname": "swift-otter",
            }]
        }),
    );

    let snapshot = tasks.snapshot().expect("registry exists");
    let child = &snapshot.tasks[0];
    assert_eq!(child.state, BackgroundTaskState::Done);
    assert_eq!(
        child.display_name.as_deref(),
        Some("swift-otter"),
        "stale responses may still fill metadata the live stream never carried"
    );
}

#[test]
fn a_failed_query_keeps_known_rows_and_only_reports_unavailable_when_empty() {
    let mut empty = rooted();
    empty.descendant_request(7, None);
    assert!(empty.fail_query(7, "thread/list unsupported"));
    assert!(matches!(
        empty.snapshot().expect("registry exists").discovery,
        BackgroundTaskDiscoveryState::Unavailable { .. }
    ));

    let mut populated = rooted();
    populated.observe_parent_item(&spawn_item("thr_child"));
    populated.descendant_request(7, None);
    populated.fail_query(7, "thread/list unsupported");

    let snapshot = populated.snapshot().expect("registry exists");
    assert_eq!(snapshot.tasks.len(), 1);
    assert_eq!(snapshot.active_count(), 1);
    assert!(matches!(
        snapshot.discovery,
        BackgroundTaskDiscoveryState::Ready
    ));
}

#[test]
fn selecting_another_root_drops_the_previous_conversation_rows() {
    let mut tasks = rooted();
    tasks.observe_parent_item(&spawn_item("thr_child"));
    assert!(tasks.set_root("thr_other_parent"));

    let snapshot = tasks.snapshot().expect("registry exists");
    assert!(snapshot.tasks.is_empty());
    assert_eq!(
        snapshot.parent_session,
        BackgroundTaskKey::codex("thr_other_parent")
    );
    assert_eq!(tasks.scope(Some("thr_child")), ThreadScope::Unrelated);
}

#[test]
fn a_repeated_pagination_cursor_ends_discovery() {
    let mut tasks = rooted();
    assert!(tasks.accept_cursor("page-2"));
    assert!(
        !tasks.accept_cursor("page-2"),
        "a cursor already requested for this root would page forever"
    );
    assert!(tasks.accept_cursor("page-3"));
}

#[test]
fn a_descendant_transcript_is_read_without_resuming_the_child() {
    let mut tasks = rooted();
    tasks.observe_parent_item(&spawn_item("thr_child"));

    let request = tasks
        .transcript_request(9, "thr_child")
        .expect("a confirmed descendant can be read");
    // `thread/read` returns stored turns without loading the thread into this
    // session, so the parent keeps its own thread, turn, and approval state.
    assert_eq!(request["method"], "thread/read");
    assert_eq!(request["params"]["threadId"], "thr_child");
    assert_eq!(request["params"]["includeTurns"], json!(true));
    assert!(tasks.is_transcript_read(9));

    assert_eq!(
        tasks.finish_transcript_read(9).as_deref(),
        Some("thr_child")
    );
    assert!(!tasks.is_transcript_read(9));
}

#[test]
fn only_confirmed_descendants_can_be_read() {
    let mut tasks = rooted();

    assert!(
        tasks.transcript_request(9, "thr_stranger").is_none(),
        "a thread this parent never spawned is not readable through it"
    );
    assert!(
        tasks.transcript_request(9, ROOT).is_none(),
        "the parent's own conversation is not a child transcript"
    );
}

#[test]
fn a_second_read_for_the_same_child_is_not_issued() {
    let mut tasks = rooted();
    tasks.observe_parent_item(&spawn_item("thr_child"));

    assert!(tasks.transcript_request(9, "thr_child").is_some());
    assert!(
        tasks.transcript_request(10, "thr_child").is_none(),
        "a read already in flight would return the same stored conversation"
    );

    tasks.finish_transcript_read(9);
    assert!(
        tasks.transcript_request(11, "thr_child").is_some(),
        "a later refresh is allowed once the previous read settled"
    );
}

#[test]
fn selecting_another_root_drops_in_flight_reads() {
    let mut tasks = rooted();
    tasks.observe_parent_item(&spawn_item("thr_child"));
    tasks.transcript_request(9, "thr_child");

    tasks.set_root("thr_other_parent");

    assert!(!tasks.is_transcript_read(9));
}

#[test]
fn a_spawn_prompt_is_the_first_item_in_a_child_transcript() {
    let mut tasks = rooted();
    tasks.observe_parent_item(&spawn_item("thr_child"));

    let items = tasks.with_launch_message(
        "thr_child",
        vec![Item::AgentMessage {
            id: "reply-1".into(),
            text: Some("review complete".into()),
        }],
    );

    assert!(matches!(
        &items[0],
        Item::UserMessage { text: Some(text) } if text == "review the diff"
    ));
}

#[test]
fn a_raw_agent_message_supplies_the_v2_launch_instruction() {
    let mut tasks = rooted();
    assert!(tasks.observe_raw_response_item(
        "thr_child",
        &json!({
            "type": "agent_message",
            "author": "/root",
            "recipient": "/root/reviewer",
            "content": [
                {"type": "input_text", "text": "inspect the updated parser"},
            ],
        }),
    ));
    tasks.observe_parent_item(&json!({
        "type": "subAgentActivity",
        "id": "activity-1",
        "kind": "started",
        "agentThreadId": "thr_child",
        "agentPath": "reviewer",
    }));
    assert!(!tasks.observe_raw_response_item(
        "thr_child",
        &json!({
            "type": "agent_message",
            "author": "/root",
            "recipient": "/root/reviewer",
            "content": [
                {"type": "input_text", "text": "a later message"},
            ],
        }),
    ));

    let items = tasks.with_launch_message("thr_child", Vec::new());
    assert_eq!(
        items,
        vec![Item::UserMessage {
            text: Some("inspect the updated parser".into()),
        }]
    );
}

#[test]
fn encrypted_agent_content_is_not_shown_as_a_partial_instruction() {
    let mut tasks = rooted();

    assert!(!tasks.observe_raw_response_item(
        "thr_child",
        &json!({
            "type": "agent_message",
            "author": "/root",
            "recipient": "/root/reviewer",
            "content": [
                {"type": "input_text", "text": "message metadata"},
                {"type": "encrypted_content", "encrypted_content": "ciphertext"},
            ],
        }),
    ));
}

#[test]
fn a_matching_stored_user_message_is_not_added_twice() {
    let mut tasks = rooted();
    tasks.observe_parent_item(&spawn_item("thr_child"));
    let existing = Item::UserMessage {
        text: Some("review the diff".into()),
    };

    let items = tasks.with_launch_message("thr_child", vec![existing.clone()]);

    assert_eq!(items, vec![existing]);
}
