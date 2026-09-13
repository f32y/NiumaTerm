use std::net::TcpListener;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::{Value, json};

use crate::background_task::BackgroundTaskTranscriptState;
use crate::chat::{Event, Item};
use crate::dsh::api::ApiClient;
use crate::dsh::models::ModelDirectory;
use crate::dsh::session::loads::{
    failed_read_events, load_agent_presets, load_commands, load_models, load_sessions, load_skills,
    load_subagent_transcript, load_subagents, load_workflow_transcript,
};
use crate::dsh::session::{
    COMMANDS_FRAME, HISTORY_FRAME, MODELS_FRAME, PRESETS_FRAME, SKILLS_FRAME,
    SUBAGENT_TRANSCRIPT_FRAME, SUBAGENTS_FRAME, WORKFLOW_TRANSCRIPT_FRAME,
};

#[test]
fn failed_background_reads_deliver_results_and_end_pending_discovery() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = ApiClient::new(format!("http://{}", listener.local_addr().unwrap())).unwrap();

    drop(listener);

    let (sender, receiver) = mpsc::channel();

    let deliver: Arc<dyn Fn(Value) + Send + Sync> = Arc::new(move |value| {
        sender.send(value).unwrap();
    });

    for kind in [
        COMMANDS_FRAME,
        SKILLS_FRAME,
        PRESETS_FRAME,
        HISTORY_FRAME,
        SUBAGENTS_FRAME,
        SUBAGENT_TRANSCRIPT_FRAME,
        WORKFLOW_TRANSCRIPT_FRAME,
        MODELS_FRAME,
    ] {
        let api = client.clone();
        let session = "session-1".to_string();
        let send = Arc::clone(&deliver);

        match kind {
            COMMANDS_FRAME => load_commands(api, session, send),
            SKILLS_FRAME => load_skills(api, session, send),
            PRESETS_FRAME => load_agent_presets(api, session, None, send),
            HISTORY_FRAME => load_sessions(api, None, send),
            SUBAGENTS_FRAME => load_subagents(api, session, 4, send),

            SUBAGENT_TRANSCRIPT_FRAME => {
                load_subagent_transcript(api, session, "child".into(), true, send)
            }

            WORKFLOW_TRANSCRIPT_FRAME => {
                load_workflow_transcript(api, "task".into(), "child".into(), send)
            }

            MODELS_FRAME => load_models(
                api,
                session,
                json!({ "provider": "deepseek", "model": "chat" }),
                None,
                None,
                false,
                send,
            ),

            _ => unreachable!(),
        }

        let frame = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        let payload = &frame["payload"];

        assert_eq!(payload["type"], kind);
        assert!(!payload["readError"].as_str().unwrap().is_empty());

        if kind == MODELS_FRAME {
            assert_eq!(
                ModelDirectory::parse(&payload["models"]).selected(),
                Some("chat")
            );

            continue;
        }

        let events = failed_read_events(payload, "session-1").unwrap();

        assert!(matches!(
            events.last(),
            Some(Event::ItemStarted(Item::Error { .. }))
        ));

        match kind {
            COMMANDS_FRAME => {
                assert!(matches!(&events[0], Event::Commands(commands) if commands.is_empty()))
            }

            SKILLS_FRAME => assert!(matches!(&events[0], Event::Skills(_))),

            SUBAGENT_TRANSCRIPT_FRAME => assert!(
                matches!(&events[0], Event::BackgroundTaskTranscript { update, .. }
                if !update.replace && matches!(update.state, Some(BackgroundTaskTranscriptState::Unavailable { .. })))
            ),

            _ => assert_eq!(
                events.len(),
                1,
                "failed reads must preserve existing content"
            ),
        }

        if payload["sessionId"].is_string() {
            assert!(
                failed_read_events(payload, "another-session")
                    .unwrap()
                    .is_empty()
            );
        }
    }
}
