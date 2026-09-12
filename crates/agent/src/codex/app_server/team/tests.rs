use serde_json::json;

use crate::chat::{Event, ThreadSettings};
use crate::codex::app_server::control::QueryKind;
use crate::codex::app_server::team::TeamState;
use crate::codex::app_server::tests::disconnected_session;
use crate::session::team_capabilities::TeamLaunch;

#[test]
fn restored_team_history_uses_completed_turn_ids_and_root_replies() {
    let mut session = disconnected_session();

    session.team = Some(TeamState::new(TeamLaunch {
        moderator: false,
        restore_transcript: false,
    }));

    session.control.track_query(42, QueryKind::Resume);

    let events = session.process(json!({"id": 42, "result": {
        "thread": {"id": "saved-thread", "turns": [
            {"id": "done", "status": "completed", "items": [
                {"type": "agentMessage", "text": "Progress"},
                {"type": "agentMessage", "text": "Final root reply"}
            ]},
            {"id": "partial", "status": "inProgress", "items": [{"type": "agentMessage", "text": "Partial"}]},
            {"id": "failed", "status": "failed", "items": [{"type": "agentMessage", "text": "Partial"}]},
            {"status": "completed", "items": []}
        ]}
    }}));

    assert!(events.iter().any(|event| matches!(event, Event::Ready(_))));

    let turns = session.team_recovered_turns();

    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].id, "done");
    assert_eq!(turns[0].text, "Final root reply");
}

#[test]
fn team_members_keep_native_settings_and_approval_requests() {
    let mut session = disconnected_session();

    let settings = ThreadSettings {
        sandbox: Some("workspaceWrite".into()),
        approval: Some("on-request".into()),
        ..ThreadSettings::default()
    };

    session.team = Some(TeamState::new(TeamLaunch {
        moderator: false,
        restore_transcript: false,
    }));

    let next_request = session.control.next_id();

    assert_eq!(
        session.finish_team_start(vec![Event::Ready(settings.clone())]),
        vec![Event::Ready(settings)]
    );
    assert_eq!(session.control.next_id(), next_request);

    let events = session.process_server_request(
        4,
        "item/fileChange/requestApproval",
        &json!({"params": {"changes": {"main.rs": {"type": "modify"}}}}),
    );

    assert!(matches!(
        events.as_slice(),
        [Event::ApprovalRequested { .. }]
    ));
    assert_eq!(session.conversation.pending_approval, Some(4));
}

#[test]
fn moderator_calls_require_the_registered_parent_and_current_provider_turn() {
    let mut session = disconnected_session();

    session.conversation.thread_id = Some("moderator".into());
    session.conversation.current_turn = Some("turn-4".into());

    let mut state = TeamState::new(TeamLaunch {
        moderator: true,
        restore_transcript: false,
    });

    state.ready = true;
    state.moderation_registered = true;
    session.team = Some(state);

    let mut params = json!({"threadId": "child", "turnId": "turn-4", "tool": "team_decide", "arguments": {"action": "report"}});

    assert!(session.process_team_decision(9, &params).is_empty());

    params["threadId"] = json!("moderator");

    let events = session.process_team_decision(10, &params);

    assert!(
        matches!(events.as_slice(), [Event::TeamDecision(request)] if request.request_id == 10 && request.provider_turn == "turn-4")
    );
    assert!(session.process_team_decision(10, &params).is_empty());

    params["turnId"] = json!("old-turn");

    assert!(session.process_team_decision(11, &params).is_empty());
}
