use std::path::PathBuf;
use std::{env, fs};

use uuid::Uuid;

pub(super) struct Scratch(pub(super) PathBuf);

impl Scratch {
    pub(super) fn new() -> Self {
        let path = env::temp_dir().join(format!("niumaterm-session-test-{}", Uuid::new_v4()));

        fs::create_dir(&path).unwrap();

        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(windows)]
#[test]
fn cli_session_starts_sends_images_and_rejects_cross_provider_recovery_without_a_window() {
    use std::path::Path;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use serde_json::Value;

    use crate::chat::{Event, SendOutcome, ThreadSettings};
    use crate::session::lifecycle::StartOutcome;
    use crate::session::{AgentKind, Backend, ImageAttachment, RecoveryIdentity, SessionRuntime};
    use crate::{AgentWorkspace, LaunchConfig};

    let scratch = Scratch::new();
    let log = scratch.0.join("input.jsonl");

    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude/session-runtime.ps1");

    let launch = LaunchConfig {
        executable: "powershell.exe".into(),
        executable_args: vec![
            "-NoProfile".into(),
            "-File".into(),
            fixture.to_string_lossy().into_owned(),
        ],
        env: vec![(
            "NMT_FAKE_STREAM_LOG".into(),
            log.to_string_lossy().into_owned(),
        )],
        ..LaunchConfig::default()
    };

    let (sender, receiver) = mpsc::channel();
    let mut runtime = SessionRuntime::default();
    let epoch = runtime.begin_start();

    let backend = Backend::spawn(
        AgentKind::Claude,
        &launch,
        &[],
        &AgentWorkspace::default(),
        Some(RecoveryIdentity::new(AgentKind::Codex, "unrelated-thread")),
        move |message| {
            let _ = sender.send(message);
        },
    )
    .unwrap();

    assert!(backend.recovery_identity().is_none());
    assert!(matches!(
        runtime.install(epoch, Ok(backend)),
        StartOutcome::Installed
    ));

    let init = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    let events = runtime.process(epoch, init).unwrap();

    assert!(events.iter().any(|event| matches!(event, Event::Ready(_))));

    runtime.ready();

    assert_eq!(
        runtime.backend().unwrap().recovery_identity(),
        Some(RecoveryIdentity::new(
            AgentKind::Claude,
            "40000000-0000-4000-8000-000000000000",
        ))
    );

    let outcome = runtime.send(|backend| {
        backend.send_user_message(
            "picture",
            &ThreadSettings::default(),
            None,
            [ImageAttachment {
                bytes: &[1, 2, 3],
                media_type: "image/png",
            }]
            .into_iter(),
            &scratch.0.join("images"),
        )
    });

    assert!(matches!(outcome, SendOutcome::StartedTurn));

    let deadline = Instant::now() + Duration::from_secs(5);

    let sent = loop {
        let input = fs::read_to_string(&log).unwrap_or_default();

        if let Some(message) = input
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|message| message["type"] == "user")
        {
            break message;
        }

        assert!(
            Instant::now() < deadline,
            "CLI did not receive the user message: {input}"
        );

        thread::sleep(Duration::from_millis(10));
    };

    let content = sent["message"]["content"].as_array().unwrap();

    assert!(
        content
            .iter()
            .any(|block| block["type"] == "text" && block["text"] == "picture")
    );
    assert!(content.iter().any(|block| block["type"] == "image"
        && block["source"]["media_type"] == "image/png"
        && block["source"]["data"] == "AQID"));
    assert!(!scratch.0.join("images").exists());

    let mut backend = runtime.retire().unwrap();

    backend.shutdown(Duration::from_secs(2), true).unwrap();

    assert!(runtime.process_exit(epoch).is_none());
}
