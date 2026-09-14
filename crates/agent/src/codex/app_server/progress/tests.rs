use std::fs;

use serde_json::json;
use tempfile::tempdir;

use crate::chat::Event;
use crate::codex::app_server::control::{ControlOperation, QueryKind};
use crate::codex::app_server::progress::{PLAN_RESTORED, goal_request, read_plan, task_list};
use crate::codex::app_server::tests::disconnected_session;
use crate::progress::TaskStatus;

#[test]
fn plan_and_goal_updates_are_scoped_and_live_state_wins_over_restore() {
    let mut session = disconnected_session();

    session.conversation.thread_id = Some("parent".into());

    session.background.set_root("parent");

    let plan = json!({"threadId": "parent", "turnId": "turn", "explanation": "Two targets", "plan": [
        {"step": "Build editor", "status": "completed"}, {"step": "Build insights", "status": "inProgress"}
    ]});

    let events = session.process(json!({"method": "turn/plan/updated", "params": plan}));

    let [Event::TaskListUpdated(tasks)] = events.as_slice() else {
        panic!("expected a task list")
    };

    assert_eq!(tasks.tally(), Some((1, 2)));
    assert_eq!(tasks.items[1].status, TaskStatus::InProgress);

    let mut foreign = plan;

    foreign["threadId"] = json!("other");

    assert!(
        session
            .process(json!({"method": "turn/plan/updated", "params": foreign}))
            .is_empty()
    );
    assert!(session.process(json!({"method": PLAN_RESTORED, "params": {"threadId": "parent", "revision": 0, "value": {"plan": []}}})).is_empty());

    session
        .control
        .track(999, ControlOperation::Query(QueryKind::Goal(0)));

    let events = session.process(json!({"method": "thread/goal/updated", "params": {"threadId": "parent", "goal": {
        "objective": "Verify builds", "status": "active", "tokensUsed": 12, "tokenBudget": 100, "timeUsedSeconds": 3
    }}}));

    let [Event::GoalUpdated(Some(goal))] = events.as_slice() else {
        panic!("expected a goal")
    };

    assert_eq!(goal.token_budget, Some(100));
    assert!(
        session
            .process(json!({"id": 999, "result": {"goal": null}}))
            .is_empty()
    );
    assert_eq!(
        session.process(json!({"method": "thread/goal/cleared", "params": {"threadId": "parent"}})),
        vec![Event::GoalUpdated(None)]
    );

    session
        .control
        .track(1000, ControlOperation::Command("goal".into()));

    let events = session.process(json!({"id": 1000, "result": {"goal": {"objective": "Older objective", "status": "active"}}}));

    assert!(matches!(
        events.as_slice(),
        [Event::SlashCommandResult { .. }]
    ));
}

#[test]
fn goal_commands_use_the_native_operations() {
    for (argument, method, status) in [
        ("", "thread/goal/get", None),
        ("clear", "thread/goal/clear", None),
        ("pause", "thread/goal/set", Some("paused")),
        ("resume", "thread/goal/set", Some("active")),
        ("Build both targets", "thread/goal/set", Some("active")),
    ] {
        let request = goal_request(7, "parent", argument);

        assert_eq!(request["method"], method);
        assert_eq!(request["params"]["threadId"], "parent");
        assert_eq!(request["params"]["status"].as_str(), status);

        if argument == "Build both targets" {
            assert_eq!(request["params"]["objective"], argument)
        }
    }
}

#[test]
fn restored_plan_uses_the_latest_accepted_update_including_empty_lists() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("rollout.jsonl");

    let initial = json!({"type": "event_msg", "payload": {"type": "plan_update", "plan": [
        {"step": "Build", "status": "pending"}
    ]}});

    let completed = json!({"type": "event_msg", "payload": {"type": "plan_update", "plan": [
        {"step": "Build", "status": "completed"}
    ]}});

    let unaccepted = json!({"type": "response_item", "payload": {"type": "function_call", "name": "update_plan", "arguments": "{\"plan\":[]}"}});

    fs::write(
        &path,
        format!("{initial}\ninvalid\n{completed}\n{unaccepted}\n"),
    )
    .unwrap();

    assert_eq!(task_list(&read_plan(&path).unwrap()).tally(), Some((1, 1)));

    let cleared = json!({"type": "event_msg", "payload": {"type": "plan_update", "plan": []}});

    fs::write(&path, format!("{initial}\n{cleared}\n")).unwrap();

    assert!(task_list(&read_plan(&path).unwrap()).items.is_empty());
}
