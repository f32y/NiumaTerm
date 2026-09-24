use std::fs;
use std::path::Path;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::tempdir;

use crate::LaunchConfig;
use crate::chat::{Event, Item, SendOutcome, SkillReference, ThreadSettings};
use crate::codex::app_server::Session;
use crate::workspace::AgentWorkspace;

fn receive_until(
    session: &mut Session,
    messages: &Receiver<Value>,
    matches: impl Fn(&Event) -> bool,
) -> Vec<Event> {
    let deadline = Instant::now() + Duration::from_secs(5);

    let mut events = Vec::new();

    loop {
        let message = messages
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("provider must advance the queued message");

        events.extend(session.process(message));

        if events.iter().any(&matches) {
            return events;
        }
    }
}

#[test]
fn a_steer_rejected_after_completion_automatically_starts_the_next_turn() {
    for completed_first in [false, true] {
        let directory = tempdir().unwrap();
        let log = directory.path().join("requests.jsonl");

        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex/steering.cmd");

        let launch = LaunchConfig {
            executable: fixture.to_string_lossy().into_owned(),
            env: vec![
                (
                    "NMT_FAKE_COMPLETE_FIRST".into(),
                    completed_first.to_string(),
                ),
                (
                    "NMT_FAKE_REQUEST_LOG".into(),
                    log.to_string_lossy().into_owned(),
                ),
            ],
            ..LaunchConfig::default()
        };

        let (tx, rx) = channel();

        let mut session = nmt_platform::runtime()
            .block_on(Session::spawn(
                &launch,
                &[],
                &AgentWorkspace::default(),
                move |message| {
                    let _ = tx.send(message);
                },
                |_| {},
            ))
            .unwrap();

        receive_until(&mut session, &rx, |event| matches!(event, Event::Ready(_)));

        assert_eq!(
            session.send_user_message_with_skill("first", &ThreadSettings::default(), None, &[],),
            SendOutcome::StartedTurn
        );

        receive_until(&mut session, &rx, |event| {
            matches!(event, Event::TurnStarted)
        });

        let skill = SkillReference {
            name: "inspect".into(),
            path: "C:/skills/inspect.md".into(),
        };

        let settings = ThreadSettings {
            model: Some("queue-model".into()),
            effort: Some("high".into()),
            ..ThreadSettings::default()
        };

        assert_eq!(
            session.send_user_message_with_skill(
                "queued",
                &settings,
                Some(&skill),
                &["C:/images/input.png".into()],
            ),
            SendOutcome::Steered
        );

        let events = receive_until(&mut session, &rx, |event| {
            matches!(event,
            Event::ItemStarted(Item::UserMessage { text: Some(text) }) if text == "queued")
        });

        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::TurnCompleted { .. }))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::TurnStarted))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::Error { .. }))
        );

        nmt_platform::runtime()
            .block_on(session.shutdown(Duration::from_secs(2), true))
            .unwrap();

        let requests: Vec<Value> = fs::read_to_string(log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();

        let starts: Vec<_> = requests
            .iter()
            .filter(|request| request["method"] == "turn/start")
            .collect();

        let steer = requests
            .iter()
            .find(|request| request["method"] == "turn/steer")
            .unwrap();

        assert_eq!(starts.len(), 2);
        assert_eq!(starts[1]["params"]["input"], steer["params"]["input"]);
        assert_eq!(starts[1]["params"]["model"], "queue-model");
        assert_eq!(starts[1]["params"]["effort"], "high");
    }
}
