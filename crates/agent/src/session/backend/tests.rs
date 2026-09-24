use crate::session::ImageAttachment;
use crate::session::backend::inline_images;

#[test]
fn inline_images_keep_byte_order_and_media_types() {
    let images = inline_images(
        [
            ImageAttachment {
                bytes: &[1, 2, 3],
                media_type: "image/png",
            },
            ImageAttachment {
                bytes: &[9, 8],
                media_type: "image/jpeg",
            },
        ]
        .into_iter(),
    );

    assert_eq!(images.len(), 2);
    assert_eq!(images[0].bytes, [1, 2, 3]);
    assert_eq!(images[0].media_type, "image/png");
    assert_eq!(images[1].bytes, [9, 8]);
    assert_eq!(images[1].media_type, "image/jpeg");
}

// The Team backend tests drive a `.cmd` fixture, so they only run where
// cmd.exe exists.
#[cfg(windows)]
mod team {
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

        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude/fake-stream-json.cmd");

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

        let mut backend = nmt_platform::runtime()
            .block_on(Backend::spawn_team(
                AgentKind::Claude,
                &launch,
                &[],
                &AgentWorkspace::default(),
                None,
                policy,
                |_| {},
            ))
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

        nmt_platform::runtime()
            .block_on(backend.shutdown(Duration::from_secs(2), true))
            .unwrap();
    }
}
