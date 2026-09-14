use std::path::Path;
use std::time::Duration;

use serde_json::json;
use tempfile::tempdir;

use crate::chat::Event;
use crate::session::team_capabilities::TeamLaunch;
use crate::session::{AgentKind, Backend};
use crate::{AgentWorkspace, LaunchConfig};

#[test]
fn claude_team_uses_the_ordinary_backend() {
    let directory = tempdir().unwrap();

    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/fake-stream-json.cmd");

    let launch = LaunchConfig {
        executable: fixture.to_string_lossy().into_owned(),
        env: vec![(
            "NMT_FAKE_STREAM_LOG".into(),
            directory
                .path()
                .join("input.jsonl")
                .to_string_lossy()
                .into_owned(),
        )],
        ..LaunchConfig::default()
    };

    let policy = TeamLaunch {
        moderator: false,
        restore_transcript: false,
    };

    let mut backend = Backend::spawn_team(
        AgentKind::Claude,
        &launch,
        &[],
        &AgentWorkspace::default(),
        None,
        policy,
        |_| {},
    )
    .unwrap_or_else(|error| panic!("Claude Team startup failed: {error}"));

    let Backend::Claude(session) = &mut backend else {
        panic!("Claude Team did not start the ordinary backend");
    };

    let events = session.process(json!({
        "type": "control_request",
        "request_id": "write-request",
        "request": {
            "subtype": "can_use_tool",
            "tool_name": "Write",
            "input": {"file_path": "test.txt", "content": "test"}
        }
    }));

    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::ApprovalRequested { .. }))
    );

    backend.shutdown(Duration::from_secs(2), true).unwrap();
}
