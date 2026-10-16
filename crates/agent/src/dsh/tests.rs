//! Frame payloads here are trimmed copies of ones a real host emitted, so a
//! mapping that passes these matches what the harness really sends, not
//! what its declarations suggest.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::chat::{Event, Item, QuestionAnswer, QuestionInput};
use crate::dsh::api::ApiClient;
use crate::dsh::history::sessions;
use crate::dsh::mapping::{EventTracker, map_frame};
use crate::dsh::session::{CloseAction, open_new_conversation, run_close_actions};
use crate::dsh::{history, mapping};
use crate::workspace::AgentWorkspace;

const SESSION: &str = "session-debb6efc";

fn session_frame(event: Value) -> Value {
    json!({
        "type": "server-request",
        "rpcId": "frame-1",
        "method": "session/event",
        "payload": { "type": "session/event", "sessionId": SESSION, "event": event },
    })
}

fn api_server(request_count: usize) -> (String, mpsc::Receiver<Value>, thread::JoinHandle<()>) {
    scripted_api_server(vec![
        r#"{"result":{"ok":true,"value":{"accepted":true}}}"#;
        request_count
    ])
}

/// A loopback API server answering each request with the next of `answers`.
fn scripted_api_server(
    answers: Vec<&'static str>,
) -> (String, mpsc::Receiver<Value>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback API server");
    let address = listener.local_addr().unwrap();
    let (request_tx, request_rx) = mpsc::channel();

    let server = thread::spawn(move || {
        for answer in answers {
            let (mut stream, _) = listener.accept().expect("accept API client");
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut content_length = 0;

            loop {
                let mut line = String::new();

                reader.read_line(&mut line).expect("read request header");

                if line == "\r\n" {
                    break;
                }

                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = value.trim().parse().expect("parse content length");
                }
            }

            let mut body = vec![0; content_length];

            reader.read_exact(&mut body).expect("read request body");

            request_tx
                .send(serde_json::from_slice(&body).expect("parse request body"))
                .unwrap();

            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                answer.len()
            )
            .expect("write API response");
        }
    });

    (format!("http://{address}"), request_rx, server)
}

#[test]
fn closing_a_session_drops_queued_work_before_cancelling_the_turn() {
    let (base, requests, server) = api_server(3);

    let client = nmt_platform::runtime()
        .block_on(ApiClient::new(base))
        .expect("create API client");

    let actions = vec![
        CloseAction::RemoveQueued("queued-1".into()),
        CloseAction::RemoveQueued("steering-2".into()),
        CloseAction::CancelTurn,
    ];

    assert_eq!(
        nmt_platform::runtime().block_on(run_close_actions(&client, SESSION, &actions)),
        Vec::<String>::new()
    );

    let requests: Vec<Value> = (0..actions.len())
        .map(|_| {
            requests
                .recv_timeout(Duration::from_secs(1))
                .expect("receive close request")
        })
        .collect();

    server.join().expect("API server should exit");

    assert_eq!(requests[0]["method"], "session/updateQueue");
    assert_eq!(
        requests[0]["payload"]["args"]["request"]["itemId"],
        "queued-1"
    );
    assert_eq!(requests[1]["method"], "session/updateQueue");
    assert_eq!(
        requests[1]["payload"]["args"]["request"]["itemId"],
        "steering-2"
    );
    assert_eq!(requests[2]["method"], "session/cancel");
    assert_eq!(
        requests[2]["payload"]["args"]["request"]["sessionId"],
        SESSION
    );
}

fn chunk(chunk: Value) -> Value {
    json!({
        "type": "assistant/chunk",
        "seq": 16,
        "data": { "turn": 1, "step": 1, "chunk": chunk },
    })
}

#[test]
fn text_and_reasoning_stream_as_separate_rows() {
    let started = map_frame(
        &session_frame(chunk(
            json!({ "type": "block-start", "index": 0, "blockType": "reasoning" }),
        )),
        SESSION,
        &mut EventTracker::default(),
    );

    assert_eq!(
        started,
        vec![Event::ItemStarted(Item::Reasoning {
            id: "1:1:0".into(),
            summary: None,
        })]
    );

    let reasoning = map_frame(
        &session_frame(chunk(
            json!({ "type": "reasoning-delta", "index": 0, "text": "thinking" }),
        )),
        SESSION,
        &mut EventTracker::default(),
    );

    assert_eq!(
        reasoning,
        vec![Event::ReasoningSummaryDelta {
            item_id: "1:1:0".into(),
            delta: "thinking".into(),
        }]
    );

    let text = map_frame(
        &session_frame(chunk(
            json!({ "type": "text-delta", "index": 1, "text": "answer" }),
        )),
        SESSION,
        &mut EventTracker::default(),
    );

    assert_eq!(
        text,
        vec![Event::AgentMessageDelta {
            item_id: "1:1:1".into(),
            delta: "answer".into(),
        }]
    );
}

#[test]
fn a_completed_message_reconciles_with_the_blocks_that_streamed() {
    // The completed message lists its blocks in the same order the chunks
    // announced, which is the only thing tying the two together.
    let events = map_frame(
        &session_frame(json!({
            "type": "assistant/message",
            "data": {
                "turn": 1,
                "step": 1,
                "message": {
                    "role": "assistant",
                    "content": [
                        { "type": "reasoning", "text": "the whole thought" },
                        { "type": "tool-call", "id": "call_1", "name": "read" },
                        { "type": "text", "text": "the whole answer" },
                    ],
                },
            },
        })),
        SESSION,
        &mut EventTracker::default(),
    );

    // The tool call sits between them and is skipped without shifting the
    // positions of the blocks around it.
    assert_eq!(
        events,
        vec![
            Event::ItemCompleted(Item::Reasoning {
                id: "1:1:0".into(),
                summary: Some("the whole thought".into()),
            }),
            Event::ItemCompleted(Item::AgentMessage {
                id: "1:1:2".into(),
                text: Some("the whole answer".into()),
                questions: None,
            }),
        ]
    );
}

#[test]
fn only_the_users_own_message_becomes_a_transcript_row() {
    let prompt = json!({
        "type": "user/message",
        "data": {
            "content": [{ "type": "text", "text": "do the thing" }],
            "source": { "kind": "user" },
        },
    });

    assert_eq!(
        map_frame(
            &session_frame(prompt),
            SESSION,
            &mut EventTracker::default()
        ),
        vec![Event::ItemStarted(Item::UserMessage {
            text: Some("do the thing".into()),
        })]
    );

    // One prompt also emits these three, and rendering them would put messages
    // the user never wrote into every turn.
    for injected in ["agent-instructions", "plugin", "skill-catalog"] {
        let frame = session_frame(json!({
            "type": "user/message",
            "data": {
                "content": [{ "type": "text", "text": "injected context" }],
                "source": { "kind": injected },
            },
        }));

        assert_eq!(
            map_frame(&frame, SESSION, &mut EventTracker::default()),
            Vec::new(),
            "{injected}"
        );
    }
}

#[test]
fn acceptance_uses_the_harness_turn_counter_in_the_correct_session() {
    let frame = session_frame(json!({"type":"turn/start", "data":{"turn":7}}));

    assert_eq!(
        map_frame(&frame, SESSION, &mut EventTracker::default()),
        vec![
            Event::TurnStarted,
            Event::ProviderTurnAccepted {
                id: "turn:7".into()
            }
        ]
    );
    assert!(map_frame(&frame, "another-session", &mut EventTracker::default()).is_empty());
}

#[test]
fn turn_end_reasons_separate_a_failure_from_a_stop() {
    let aborted = json!({
        "type": "turn/end",
        "data": { "turn": 1, "reason": { "kind": "aborted", "reason": { "kind": "user" } } },
    });

    assert_eq!(
        map_frame(
            &session_frame(aborted),
            SESSION,
            &mut EventTracker::default()
        ),
        vec![
            Event::TurnCompleted { error: None },
            Event::ProviderTurnFinished {
                id: "turn:1".into(),
                error: Some("The provider turn was interrupted.".into())
            }
        ]
    );

    let completed = json!({
        "type": "turn/end",
        "data": { "turn": 1, "reason": { "kind": "completed" } },
    });

    assert_eq!(
        map_frame(
            &session_frame(completed),
            SESSION,
            &mut EventTracker::default()
        ),
        vec![
            Event::TurnCompleted { error: None },
            Event::ProviderTurnFinished {
                id: "turn:1".into(),
                error: None
            }
        ]
    );

    let failed = json!({
        "type": "turn/end",
        "data": { "turn": 1, "reason": { "kind": "failed", "message": "NO_ADAPTER" } },
    });

    assert_eq!(
        map_frame(
            &session_frame(failed),
            SESSION,
            &mut EventTracker::default()
        ),
        vec![
            Event::TurnCompleted {
                error: Some("NO_ADAPTER".into()),
            },
            Event::ProviderTurnFinished {
                id: "turn:1".into(),
                error: Some("NO_ADAPTER".into())
            }
        ]
    );
}

#[test]
fn frames_for_another_session_are_ignored() {
    // The stream is aggregated across every attached session and replays each
    // one when it opens, so a tab sees other tabs' activity constantly.
    let frame = session_frame(json!({
        "type": "assistant/chunk",
        "data": { "turn": 1, "step": 1, "chunk": { "type": "text-delta", "index": 0, "text": "x" } },
    }));

    assert_eq!(
        map_frame(&frame, "session-someone-else", &mut EventTracker::default()),
        Vec::new()
    );
}

#[test]
fn unknown_types_produce_nothing_rather_than_failing() {
    // The harness adds event types between releases, and one this build has
    // never seen must not break a tab.
    let unknown_event = session_frame(json!({
        "type": "quantum/entanglement",
        "data": { "turn": 1 },
    }));

    assert_eq!(
        map_frame(&unknown_event, SESSION, &mut EventTracker::default()),
        Vec::new()
    );

    let unknown_frame = json!({
        "type": "server-request",
        "payload": { "type": "session/telepathy", "sessionId": SESSION },
    });

    assert_eq!(
        map_frame(&unknown_frame, SESSION, &mut EventTracker::default()),
        Vec::new()
    );

    // A chunk kind that is not one of the two that stream text.
    let unknown_chunk = session_frame(chunk(
        json!({ "type": "signature-delta", "index": 0, "text": "x" }),
    ));

    assert_eq!(
        map_frame(&unknown_chunk, SESSION, &mut EventTracker::default()),
        Vec::new()
    );
}

#[test]
fn host_and_stream_failures_reach_the_transcript() {
    let agent_error = json!({
        "type": "server-request",
        "payload": {
            "type": "host/agent-error",
            "sessionId": SESSION,
            "message": "the model provider refused the request",
        },
    });

    assert_eq!(
        map_frame(&agent_error, SESSION, &mut EventTracker::default()),
        vec![Event::ItemStarted(Item::Error {
            text: "the model provider refused the request".into(),
        })]
    );

    // An agent error for another tab's session belongs to that tab.
    assert_eq!(
        map_frame(&agent_error, "session-other", &mut EventTracker::default()),
        Vec::new()
    );

    let stream_error = json!({
        "type": "server-request",
        "payload": {
            "type": "stream/error",
            "error": { "code": "internal", "message": "the stream ended" },
        },
    });

    assert_eq!(
        map_frame(&stream_error, SESSION, &mut EventTracker::default()),
        vec![Event::ItemStarted(Item::Error {
            text: "the stream ended".into(),
        })]
    );
}

#[test]
fn an_unresolvable_harness_is_reported_as_missing_rather_than_as_a_failed_start() {
    use crate::LaunchConfig;
    use crate::dsh::{HostError, Session};

    // The two failures have different answers for the user, so the adapter has
    // to tell them apart before any process is spawned.
    let launch = LaunchConfig {
        executable: "dsh-that-is-not-installed".to_string(),
        ..LaunchConfig::default()
    };

    assert!(matches!(
        nmt_platform::runtime().block_on(Session::create(
            &launch,
            &AgentWorkspace::default(),
            |_| {}
        )),
        Err(HostError::NotInstalled(_))
    ));
}

#[test]
fn an_approval_request_holds_what_answering_it_needs() {
    use crate::dsh::mapping::approval_request;

    // Shape copied from a real blocked turn: the harness waits here, so a
    // client that cannot recognize this leaves the agent stalled with no
    // visible reason.
    let frame = json!({
        "type": "server-request",
        "clientId": "generation-1",
        "eventId": "3fcb9bcf-614d-414e-9041-ada82f9a0fad",
        "method": "approval/requested",
        "payload": {
            "type": "approval/requested",
            "sessionId": SESSION,
            "approvalId": "5cc446e3-8026-44f7-9fc5-e62d5213d18a",
            "toolName": "pwsh",
            "callId": "call_00_pv6xOxJBe6Cqa5rx1Xkb7091",
            "reason": "escalate sandbox to danger-full-access: the target is outside the workspace",
        },
    });

    let request = approval_request(&frame, SESSION).expect("the request should be recognized");

    assert_eq!(request.event_id, "3fcb9bcf-614d-414e-9041-ada82f9a0fad");
    assert_eq!(request.client_id, "generation-1");
    assert!(
        request.description.contains("pwsh"),
        "{}",
        request.description
    );
    assert!(
        request.description.contains("outside the workspace"),
        "{}",
        request.description
    );

    // Another tab's question belongs to that tab; answering it here would
    // resolve a decision this user never saw.
    assert_eq!(approval_request(&frame, "session-other"), None);
}

#[test]
fn only_a_successful_permission_switch_names_a_preset_to_remember() {
    use crate::chat::SlashCommandOutcome;
    use crate::dsh::catalogs;

    let success = json!({ "commandId": "cmd-1", "result": { "kind": "success", "text": "ok" } });

    let approval =
        |name: &str, arguments: &str| match catalogs::command_outcome(name, arguments, &success) {
            SlashCommandOutcome::Completed { approval, .. } => approval,
            outcome => panic!("a success result must complete: {outcome:?}"),
        };

    assert_eq!(
        approval("permission", " danger-full-access "),
        Some("danger-full-access".to_string())
    );

    // A bare command reports the preset in effect and switches nothing, and
    // another command's argument is not a preset.
    assert_eq!(approval("permission", "  "), None);
    assert_eq!(approval("compact", "danger-full-access"), None);
}

#[test]
fn a_workflow_run_is_folded_from_its_own_increments() {
    use crate::dsh::workflows::WorkflowTracker;
    use crate::workflow::{WorkflowAgentState, WorkflowRunState};

    let mut workflows = WorkflowTracker::default();

    let event = |value: Value| value;

    assert!(workflows.apply(&event(json!({
        "type": "tool-workflow/run-start",
        "data": { "runId": "wf-1", "name": "review-changes" },
    }))));
    assert_eq!(
        workflows.snapshot(SESSION).runs[0].state,
        WorkflowRunState::Starting
    );

    for (seq, label, phase) in [(1, "review:bugs", "Review"), (2, "review:perf", "Review")] {
        assert!(workflows.apply(&event(json!({
            "type": "tool-workflow/agent-start",
            "data": {
                "runId": "wf-1",
                "seq": seq,
                "label": label,
                "phase": phase,
                "childId": format!("child-{seq}"),
            },
        }))));
    }

    let run = workflows.snapshot(SESSION).runs.remove(0);

    // A member is published only once its session exists, so the run is under
    // way as soon as one arrives.
    assert_eq!(run.state, WorkflowRunState::Running);
    assert_eq!(run.agents.len(), 2);
    assert_eq!(run.agents[0].agent_id.as_deref(), Some("child-1"));

    // A member names its group by title alone, so the list is built as members
    // first mention one and both share the entry.
    assert_eq!(run.phases.len(), 1);
    assert_eq!(run.phases[0].title, "Review");
    assert_eq!(run.agents[1].phase_index, Some(0));

    assert!(workflows.apply(&event(json!({
        "type": "tool-workflow/agent-end",
        "data": { "runId": "wf-1", "seq": 1, "outcome": "failed" },
    }))));
    assert!(workflows.apply(&event(json!({
        "type": "tool-workflow/run-end",
        "data": { "runId": "wf-1", "stopReason": "error" },
    }))));

    let run = workflows.snapshot(SESSION).runs.remove(0);

    assert_eq!(run.state, WorkflowRunState::Failed);
    assert_eq!(run.agents[0].state, WorkflowAgentState::Failed);

    // The second member never reported an ending of its own, and leaving it
    // Running under a finished run would claim work still in progress.
    assert_eq!(run.agents[1].state, WorkflowAgentState::Stopped);

    // A log can be read more than once, and a repeat is not a second run.
    assert!(!workflows.apply(&event(json!({
        "type": "tool-workflow/run-start",
        "data": { "runId": "wf-1", "name": "review-changes" },
    }))));
    assert_eq!(workflows.snapshot(SESSION).runs.len(), 1);
}

#[test]
fn the_session_list_offers_only_what_this_tab_can_continue() {
    use std::time::{Duration, UNIX_EPOCH};

    use crate::dsh::history;

    let listed = json!({
        "items": [
            {
                "sessionId": "s-1",
                "updatedAt": 1_770_000_000_000u64,
                "running": false,
                "blank": false,
                "cwd": "C:/Workspace/NiumaTerm",
                "projections": { "asOfSeq": 40, "values": { "title": "Map the harness" } },
            },
            // Never ran a turn, so there is nothing to continue into.
            { "sessionId": "s-2", "updatedAt": 1, "running": false, "blank": true, "cwd": "C:/Workspace/NiumaTerm" },
            // Belongs to a parent conversation, not to this list.
            {
                "sessionId": "s-3",
                "updatedAt": 2,
                "running": false,
                "blank": false,
                "origin": "subagent",
                "cwd": "C:/Workspace/NiumaTerm",
            },
            // Another project entirely.
            { "sessionId": "s-4", "updatedAt": 3, "running": false, "blank": false, "cwd": "C:/Other" },
            // Real, but too new to have been titled.
            { "sessionId": "s-5", "updatedAt": 4, "running": false, "blank": false, "cwd": "C:/Workspace/NiumaTerm" },
        ],
    });

    let sessions = history::sessions(&listed, Some("C:/Workspace/NiumaTerm"));
    let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();

    assert_eq!(ids, vec!["s-1", "s-5"]);
    assert_eq!(sessions[0].title, "Map the harness");
    assert_eq!(
        sessions[0].last_active,
        UNIX_EPOCH + Duration::from_millis(1_770_000_000_000)
    );

    // An untitled row still names what picking it will open.
    assert_eq!(sessions[1].title, "s-5");
}

#[test]
fn a_replayed_page_rebuilds_turns_from_the_same_events_the_stream_delivers() {
    use crate::dsh::history;

    let entry = |event: Value| json!({ "event": event });

    let page = json!({
        "hasMore": false,
        "records": [
            entry(json!({ "type": "turn/start", "seq": 1, "time": 1_770_000_000_000u64, "data": { "turn": 1 } })),
            entry(json!({
                "type": "user/message",
                "seq": 2,
                "time": 1_770_000_000_000u64,
                "data": {
                    "content": [{ "type": "text", "text": "do the thing" }],
                    "source": { "kind": "user" },
                },
            })),
            entry(json!({
                "type": "assistant/chunk",
                "seq": 3,
                "time": 1_770_000_001_000u64,
                "data": { "turn": 1, "step": 1, "chunk": { "type": "block-start", "index": 0, "blockType": "text" } },
            })),
            entry(json!({
                "type": "assistant/message",
                "seq": 4,
                "time": 1_770_000_004_000u64,
                "data": {
                    "turn": 1,
                    "step": 1,
                    "message": { "role": "assistant", "content": [{ "type": "text", "text": "done" }] },
                },
            })),
            entry(json!({
                "type": "turn/end",
                "seq": 5,
                "time": 1_770_000_009_000u64,
                "data": { "turn": 1, "reason": { "kind": "aborted", "reason": { "kind": "user" } } },
            })),
        ],
    });

    let turns = history::replay(&page);

    assert_eq!(turns.len(), 1);

    // The item stream cannot express either, so both come from the boundary
    // events themselves.
    assert!(turns[0].interrupted);
    assert_eq!(turns[0].seconds, Some(9));

    // The streamed row and its completed payload are one row, not two.
    let items: Vec<&Item> = turns[0].items.iter().map(|entry| &entry.item).collect();

    assert_eq!(
        items,
        vec![
            &Item::UserMessage {
                text: Some("do the thing".into()),
            },
            &Item::AgentMessage {
                id: "1:1:0".into(),
                text: Some("done".into()),
                questions: None,
            },
        ]
    );
    assert_eq!(turns[0].items[0].at, Some(1_770_000_000));
}

#[test]
fn a_compaction_records_itself_only_once_it_produced_a_summary() {
    use crate::chat::{Compaction, CompactionTrigger};

    let start = session_frame(json!({
        "type": "compaction/start",
        "data": { "compactionId": "cmp-1", "turn": 3 },
    }));

    assert_eq!(
        map_frame(&start, SESSION, &mut EventTracker::default()),
        vec![Event::CompactionStarted]
    );

    let summary = session_frame(json!({
        "type": "compaction/summary",
        "data": {
            "compactionId": "cmp-1",
            "sourceCommandId": "cmd-9",
            "summary": [{ "type": "text", "text": "what happened so far" }],
            "shadowedRange": { "start": 4, "end": 40 },
            "shadowedSeqs": [4, 9, 12],
            "shadowedTokenCount": 8400,
            "provider": "deepseek",
            "model": "deepseek-chat",
        },
    }));

    assert_eq!(
        map_frame(&summary, SESSION, &mut EventTracker::default()),
        vec![Event::ItemCompleted(Item::Compaction {
            id: "cmp-1".into(),
            detail: Compaction {
                // The command that asked for it is what makes it manual.
                trigger: Some(CompactionTrigger::Manual),
                pre_tokens: Some(8400),
                post_tokens: None,
                messages_summarized: Some(3),
                user_context: None,
                summary: Some("what happened so far".into()),
            },
        })]
    );

    let end = session_frame(json!({
        "type": "compaction/end",
        "data": { "compactionId": "cmp-1", "turn": 3, "error": "the summarizer failed" },
    }));

    assert_eq!(
        map_frame(&end, SESSION, &mut EventTracker::default()),
        vec![Event::CompactionFinished {
            error: Some("the summarizer failed".into()),
        }]
    );

    // The replacement the conversation continues from rides a checkpoint
    // source, so it must not read as something the user typed.
    let replacement = session_frame(json!({
        "type": "user/message",
        "data": {
            "content": [{ "type": "text", "text": "what happened so far" }],
            "source": { "kind": "plugin", "plugin": "compact", "compactionId": "cmp-1" },
        },
    }));

    assert_eq!(
        map_frame(&replacement, SESSION, &mut EventTracker::default()),
        Vec::new()
    );
}

#[test]
fn the_model_directory_addresses_a_pick_as_a_provider_and_model_pair() {
    use crate::dsh::models::ModelDirectory;

    let directory = ModelDirectory::parse(&json!({
        "current": { "provider": "deepseek", "model": "deepseek-chat", "reasoningEffort": "high" },
        "routable": true,
        "groups": [
            {
                "id": "deepseek",
                "name": "DeepSeek",
                "models": [
                    {
                        "id": "deepseek-chat",
                        "name": "DeepSeek Chat",
                        "reasoning": {
                            "efforts": [{ "id": "low", "name": "Low" }, { "id": "high", "name": "High" }],
                            "defaultEffort": "low",
                        },
                    },
                    { "id": "deepseek-coder", "name": "DeepSeek Coder" },
                ],
            },
            {
                "id": "openrouter",
                "name": "OpenRouter",
                "models": [{ "id": "deepseek-chat", "name": "DeepSeek Chat" }],
            },
        ],
        "failures": [],
    }));

    let catalog = directory.catalog();
    let keys: Vec<&str> = catalog.iter().map(|m| m.model.as_str()).collect();

    // A model id two providers serve cannot address either one alone, while an
    // id only one serves stays bare so a profile can name it the plain way.
    assert_eq!(
        keys,
        vec![
            "deepseek/deepseek-chat",
            "deepseek-coder",
            "openrouter/deepseek-chat",
        ]
    );
    assert_eq!(catalog[0].display, "DeepSeek Chat (DeepSeek)");
    assert_eq!(catalog[1].display, "DeepSeek Coder");
    assert_eq!(
        catalog[0].efforts,
        vec!["low".to_string(), "high".to_string()]
    );
    assert!(catalog[1].efforts.is_empty());

    assert_eq!(directory.selected(), Some("deepseek/deepseek-chat"));
    assert_eq!(directory.effort(), Some("high"));
    assert_eq!(
        directory.route("openrouter/deepseek-chat"),
        ("openrouter", "deepseek-chat")
    );

    // A custom id the catalog never listed still routes, through the provider
    // the session is already on.
    assert_eq!(
        directory.route("nothing-like-this"),
        ("deepseek", "nothing-like-this")
    );

    // A slash inside a model id names no provider, so it survives the split.
    assert_eq!(
        directory.route("Qwen/Qwen3-32B"),
        ("deepseek", "Qwen/Qwen3-32B")
    );
}

#[test]
fn declaring_image_input_rewrites_the_catalog_the_harness_already_serves() {
    use crate::dsh::session::models_with_image;

    // A settings write replaces the value at its path, so every model the
    // harness already serves has to travel with the one being declared.
    let catalog = json!([
        { "id": "deepseek-v4-flash", "name": "DeepSeek-V4-Flash", "contextWindow": 128_000 },
        { "id": "custom-vision", "name": "Custom", "inputModalities": ["text"] },
    ]);

    let written = models_with_image(&catalog, "custom-vision", "inputModalities")
        .expect("a text-only model changes");

    assert_eq!(written[0], catalog[0]);
    assert_eq!(written[1]["inputModalities"], json!(["text", "image"]));

    // The entry keeps what the harness knew about it; only its modalities move.
    assert_eq!(written[1]["name"], json!("Custom"));

    // A model the catalog never listed holds only what is being declared.
    let appended = models_with_image(&catalog, "proxy-model", "inputModalities")
        .expect("an unlisted model changes");

    assert_eq!(
        appended[2],
        json!({ "id": "proxy-model", "inputModalities": ["text", "image"] })
    );

    // Nothing to write is reported as nothing to write, so a tab that opens
    // twice does not churn the user's configuration file.
    assert!(models_with_image(&written, "custom-vision", "inputModalities").is_none());

    // The OpenAI-compatible adapter spells the same declaration `input`, and
    // reading the wrong field would rewrite a catalog that is already right.
    let pi_ai = json!([{ "id": "custom-vision", "input": ["text", "image"] }]);

    assert!(models_with_image(&pi_ai, "custom-vision", "input").is_none());
    assert_eq!(
        models_with_image(&pi_ai, "other", "input").expect("an unlisted model changes")[1],
        json!({ "id": "other", "input": ["text", "image"] })
    );
}

/// One projection unit's whole current value.
fn projection_frame(key: &str, value: Value) -> Value {
    json!({
        "type": "server-request",
        "payload": {
            "type": "session/projection",
            "sessionId": SESSION,
            "key": key,
            "value": value,
            "seq": 42,
        },
    })
}

#[test]
fn usage_projections_combine_into_one_window_snapshot() {
    use crate::chat::ContextUsageScope;
    use crate::dsh::projections::ProjectionTracker;

    let mut usage = ProjectionTracker::default();

    // Cumulative totals alone draw no bar: the occupancy they would be shown
    // beside is not known until the provider reports a request.
    assert_eq!(
        usage.apply(
            &projection_frame(
                "tokenUsage",
                json!({
                    "uncachedInputTokens": 1200,
                    "outputTokens": 300,
                    "cacheReadTokens": 8000,
                    "cacheWriteTokens": 500,
                }),
            ),
            SESSION,
        ),
        Some(Vec::new())
    );

    let events = usage
        .apply(
            &projection_frame(
                "contextPressure",
                json!({ "pressureTokens": 9700, "projectedTokens": 9950, "contextWindow": 128000 }),
            ),
            SESSION,
        )
        .expect("a projection frame for this session should be claimed");

    let [Event::ContextWindowUpdated(window)] = events.as_slice() else {
        panic!("expected one window snapshot, got {events:?}");
    };

    // The projected figure is what reacts to a compaction, so it wins over the
    // provider's older sample.
    assert_eq!(window.current.total_tokens, 9950);
    assert_eq!(window.max_tokens, Some(128_000));

    let cumulative = window.cumulative.expect("the totals should ride along");

    assert_eq!(cumulative.scope, ContextUsageScope::Thread);
    assert_eq!(cumulative.breakdown.total_tokens, 10_000);
    assert_eq!(cumulative.breakdown.cache_read_input_tokens, Some(8000));

    // Cache traffic belongs inside the input figure, so a reader dividing the
    // read count by it gets a share at or below 100%.
    assert_eq!(cumulative.breakdown.input_tokens, Some(9700));

    // Another tab's projection is not this tab's accounting.
    assert_eq!(
        usage.apply(&projection_frame("tokenUsage", json!({})), "session-other"),
        None
    );
}

#[test]
fn the_permission_presets_come_from_the_deployment_rather_than_from_here() {
    use crate::dsh::models::ModelDirectory;
    use crate::dsh::projections::ProjectionTracker;
    use crate::dsh::session::ready_settings;

    let mut projections = ProjectionTracker::default();

    let events = projections
        .apply(
            &projection_frame(
                "permissions",
                json!({
                    "options": [
                        { "value": "read-only", "name": "Read Only", "description": "No writes" },
                        { "value": "workspace-write", "name": "Workspace Write" },
                        { "value": "custom", "name": "Custom" },
                    ],
                    "currentValue": "custom",
                }),
            ),
            SESSION,
        )
        .expect("a projection frame for this session should be claimed");

    let [Event::ApprovalPresets { presets, current }] = events.as_slice() else {
        panic!("expected one preset snapshot, got {events:?}");
    };

    assert_eq!(presets.len(), 3);
    assert_eq!(presets[0].value, "read-only");
    assert_eq!(presets[0].label, "Read Only");
    assert_eq!(presets[0].description.as_deref(), Some("No writes"));

    // The derived entry is offered only while it is what the session is on.
    assert_eq!(current.as_deref(), Some("custom"));

    let models = ModelDirectory::parse(&json!({
        "current": {
            "provider": "deepseek-official",
            "model": "deepseek-chat",
            "reasoningEffort": "max",
        },
        "groups": [],
    }));

    let settings = ready_settings(&models, &projections);

    assert_eq!(settings.model.as_deref(), Some("deepseek-chat"));
    assert_eq!(settings.approval.as_deref(), Some("custom"));
    assert_eq!(settings.effort.as_deref(), Some("max"));
}

#[test]
fn a_0_2_permission_picker_combines_the_catalog_with_the_current_value() {
    use crate::dsh::projections::ProjectionTracker;

    let mut projections = ProjectionTracker::default();

    // The current value alone has no table to show it in.
    let events = projections
        .apply(
            &projection_frame("permissions", json!({ "currentValue": "workspace-write" })),
            SESSION,
        )
        .unwrap();

    assert!(events.is_empty());
    assert_eq!(projections.permission(), Some("workspace-write"));

    let events = projections.apply_permission_catalog(&json!({
        "options": [
            { "value": "workspace-write", "name": "Workspace Write" },
            { "value": "danger-full-access", "name": "Full Access" },
        ],
        "defaultOptions": [],
        "defaultPreset": "workspace-write",
    }));

    let [Event::ApprovalPresets { presets, current }] = events.as_slice() else {
        panic!("expected one preset snapshot, got {events:?}");
    };

    assert_eq!(presets.len(), 2);
    assert_eq!(current.as_deref(), Some("workspace-write"));

    // A later switch keeps the table the catalog supplied.
    let events = projections
        .apply(
            &projection_frame(
                "permissions",
                json!({ "currentValue": "danger-full-access" }),
            ),
            SESSION,
        )
        .unwrap();

    assert!(matches!(
        events.as_slice(),
        [Event::ApprovalPresets { presets, current }]
            if presets.len() == 2 && current.as_deref() == Some("danger-full-access")
    ));
}

#[test]
fn the_history_page_baseline_seeds_what_a_live_push_would_not() {
    use crate::dsh::projections::ProjectionTracker;

    // A push reports only what changed after the tab attached, so a session
    // that has been running since before it opened would show nothing.
    let mut projections = ProjectionTracker::default();

    let events = projections.apply_baseline(
        &json!({
            "contextPressure": { "projectedTokens": 4200, "contextWindow": 64000 },
            "title": "Map the harness",
        }),
        None,
    );

    let [
        Event::ContextWindowUpdated(window),
        Event::TitleUpdated(title),
    ] = events.as_slice()
    else {
        panic!("expected a window snapshot and a title, got {events:?}");
    };

    assert_eq!(window.current.total_tokens, 4200);
    assert_eq!(window.max_tokens, Some(64_000));

    // The harness names a conversation itself, and a tab that read only live
    // pushes would keep its own placeholder until the name happened to change.
    assert_eq!(title, "Map the harness");
}

#[test]
fn a_conversation_still_waiting_for_a_name_keeps_the_one_it_shows() {
    use crate::dsh::projections::ProjectionTracker;

    // The unit is registered from the moment a session exists and reports null
    // until the titler has something to work from, so the absent case is a
    // value that arrives, not a key that stays missing.
    let mut projections = ProjectionTracker::default();

    assert!(
        projections
            .apply_baseline(&json!({ "title": Value::Null }), None)
            .is_empty()
    );
    assert!(
        projections
            .apply_baseline(&json!({ "title": "   " }), None)
            .is_empty()
    );
}

#[test]
fn a_question_request_holds_the_ids_an_answer_is_matched_against() {
    use crate::dsh::mapping::question_request;

    let frame = json!({
        "type": "server-request",
        "clientId": "generation-2",
        "eventId": "0f21a6f2-7f52-4b0f-bb2f-9c0e9d2f0a11",
        "method": "question/requested",
        "payload": {
            "type": "question/requested",
            "sessionId": SESSION,
            "questions": [
                {
                    "id": "q1",
                    "question": "Which database?",
                    "detail": "The schema is already written for Postgres.",
                    "header": "Storage",
                    "options": [
                        { "label": "Postgres", "description": "What the schema targets" },
                        { "label": "SQLite" },
                    ],
                },
                {
                    "id": "q2",
                    "question": "Which extras?",
                    "multiSelect": true,
                    "options": [{ "label": "Metrics" }, { "label": "Tracing" }],
                },
            ],
        },
    });

    let (request, questions) =
        question_request(&frame, SESSION).expect("the request should be recognized");

    assert_eq!(request.event_id, "0f21a6f2-7f52-4b0f-bb2f-9c0e9d2f0a11");
    assert_eq!(request.client_id, "generation-2");

    // The harness matches each answer against the question at the same
    // position, so the ask order is what makes the batch answerable.
    assert_eq!(request.ids, vec!["q1".to_string(), "q2".to_string()]);

    assert_eq!(questions[0].header.as_deref(), Some("Storage"));
    assert!(
        questions[0].question.contains("Postgres"),
        "the detail belongs in front of the user: {}",
        questions[0].question
    );
    assert!(!questions[0].multi_select);
    assert_eq!(questions[0].options[0].label, "Postgres");
    assert_eq!(
        questions[0].options[0].description.as_deref(),
        Some("What the schema targets")
    );
    assert!(questions[1].multi_select);

    // The harness answers with the picked labels and, separately, the text the
    // user typed where the labels did not cover the answer, so the card has to
    // offer somewhere to type.
    assert_eq!(questions[0].input, QuestionInput::Text);
    assert_eq!(questions[1].input, QuestionInput::Text);

    // Another tab's question belongs to that tab.
    assert_eq!(question_request(&frame, "session-other"), None);
}

#[test]
fn a_typed_question_answer_is_reported_apart_from_the_picked_labels() {
    use crate::dsh::session::question_answer;

    // A pick is the labels the harness matches against its own options.
    assert_eq!(
        question_answer(
            "q1",
            &QuestionAnswer {
                selected: vec!["Postgres".into()],
                custom: None,
            }
        ),
        json!({ "id": "q1", "selected": ["Postgres"] })
    );

    // Text the options did not cover is the field the harness reads as the
    // user's own answer. It is left out, not sent empty, when the user only
    // picked.
    assert_eq!(
        question_answer(
            "q1",
            &QuestionAnswer {
                selected: Vec::new(),
                custom: Some("neither, use SQLite in WAL mode".into()),
            }
        ),
        json!({
            "id": "q1",
            "selected": [],
            "custom": "neither, use SQLite in WAL mode",
        })
    );
}

#[test]
fn a_resolved_approval_takes_the_card_down() {
    let frame = json!({
        "type": "server-request",
        "payload": {
            "type": "approval/resolved",
            "sessionId": SESSION,
            "approvalId": "5cc446e3",
            "outcome": "allowed-once",
        },
    });

    assert_eq!(
        map_frame(&frame, SESSION, &mut EventTracker::default()),
        vec![Event::ApprovalResolved]
    );
    assert_eq!(
        map_frame(&frame, "session-other", &mut EventTracker::default()),
        Vec::new()
    );
}

/// A tool frame holds the host-computed card alongside the logged event.
fn tool_frame(event: Value, view: Value) -> Value {
    json!({
        "type": "server-request",
        "payload": {
            "type": "session/event",
            "sessionId": SESSION,
            "event": event,
            "view": view,
        },
    })
}

#[test]
fn a_shell_command_becomes_a_command_row_with_its_output_and_exit_code() {
    let mut tools = EventTracker::default();

    let started = map_frame(
        &tool_frame(
            json!({
                "type": "tool/call",
                "data": { "turn": 1, "step": 1, "callId": "call_1", "name": "pwsh" },
            }),
            json!({ "for": "call", "view": {
                "card": "terminal",
                "title": "echo hello",
                "description": "Greet",
            }}),
        ),
        SESSION,
        &mut tools,
    );

    assert_eq!(
        started,
        vec![Event::ItemStarted(Item::CommandExecution {
            id: "call_1".into(),
            command: "echo hello".into(),
            purpose: Some("Greet".into()),
            aggregated_output: None,
            status: Some("inProgress".into()),
            exit_code: None,
        })]
    );

    let completed = map_frame(
        &tool_frame(
            json!({
                "type": "tool/result",
                "data": { "message": {
                    "source": { "kind": "tool", "callId": "call_1" },
                    "content": [{ "type": "tool-result", "toolCallId": "call_1", "isError": false,
                                  "content": [{ "type": "text", "text": "hello" }] }],
                }},
            }),
            json!({ "for": "result", "view": {
                "card": "terminal",
                "output": "hello\n",
                "exitCode": 0,
            }}),
        ),
        SESSION,
        &mut tools,
    );

    // The command line survives from the call: the result names only the id.
    assert_eq!(
        completed,
        vec![Event::ItemCompleted(Item::CommandExecution {
            id: "call_1".into(),
            command: "echo hello".into(),
            purpose: Some("Greet".into()),
            aggregated_output: Some("hello\n".into()),
            status: Some("completed".into()),
            exit_code: Some(0),
        })]
    );
}

#[test]
fn a_0_2_tool_role_result_completes_its_row_with_the_tool_output() {
    let mut tools = EventTracker::default();

    for call in ["call_ok", "call_failed"] {
        map_frame(
            &tool_frame(
                json!({
                    "type": "tool/call",
                    "data": { "turn": 1, "step": 1, "callId": call, "name": "pwsh" },
                }),
                json!({ "for": "call", "view": { "card": "terminal", "title": "probe" }}),
            ),
            SESSION,
            &mut tools,
        );
    }

    let mut result = |call: &str, text: &str, failed: bool| {
        map_frame(
            &tool_frame(
                json!({
                    "type": "tool/result",
                    "data": { "message": {
                        "role": "tool", "toolCallId": call, "isError": failed,
                        "source": { "kind": "tool", "callId": call },
                        "content": [{ "type": "text", "text": text }],
                    }},
                }),
                Value::Null,
            ),
            SESSION,
            &mut tools,
        )
    };

    assert!(matches!(
        result("call_ok", "tool-probe-ok", false).as_slice(),
        [Event::ItemCompleted(Item::CommandExecution { aggregated_output: Some(output), status: Some(status), .. })]
            if output == "tool-probe-ok" && status == "completed"
    ));

    assert!(matches!(
        result("call_failed", "denied", true).as_slice(),
        [Event::ItemCompleted(Item::CommandExecution { status: Some(status), .. })]
            if status == "failed"
    ));
}

#[test]
fn an_edit_becomes_a_file_row_whose_result_diff_includes_context() {
    let mut tools = EventTracker::default();

    let started = map_frame(
        &tool_frame(
            json!({
                "type": "tool/call",
                "data": { "turn": 1, "step": 2, "callId": "call_2", "name": "edit" },
            }),
            json!({ "for": "call", "view": {
                "card": "diff",
                "title": "Edit probe-target.txt",
                "diffs": [{ "path": "probe-target.txt", "oldText": "before", "newText": "after" }],
            }}),
        ),
        SESSION,
        &mut tools,
    );

    let Some(Event::ItemStarted(Item::FileChange { paths, diff, .. })) = started.first() else {
        panic!("a diff card should open a file row, got {started:?}");
    };

    assert_eq!(paths, "probe-target.txt");
    assert!(diff.as_deref().unwrap_or_default().contains("-before"));

    let completed = map_frame(
        &tool_frame(
            json!({
                "type": "tool/result",
                "data": { "message": {
                    "source": { "kind": "tool", "callId": "call_2" },
                    "content": [{ "type": "tool-result", "toolCallId": "call_2", "isError": false,
                                  "content": [{ "type": "text", "text": "ok" }] }],
                }},
            }),
            json!({ "for": "result", "view": {
                "card": "diff",
                "diffs": [{ "path": "probe-target.txt",
                            "oldText": "line one\nbefore\nline three",
                            "newText": "line one\nafter\nline three" }],
            }}),
        ),
        SESSION,
        &mut tools,
    );

    let Some(Event::ItemCompleted(Item::FileChange { diff, status, .. })) = completed.first()
    else {
        panic!("the result should complete the file row, got {completed:?}");
    };

    // The result diff includes surrounding lines the arguments never had.
    let body = diff.as_deref().unwrap_or_default();

    assert!(body.contains("-line one"), "{body}");
    assert!(body.contains("+after"), "{body}");
    assert_eq!(status.as_deref(), Some("completed"));
}

#[test]
fn a_failed_call_reports_the_text_the_model_saw() {
    let mut tools = EventTracker::default();

    map_frame(
        &tool_frame(
            json!({
                "type": "tool/call",
                "data": { "callId": "call_4", "name": "pwsh" },
            }),
            json!({ "for": "call", "view": { "card": "terminal", "title": "write outside" }}),
        ),
        SESSION,
        &mut tools,
    );

    // A failed call produces no result view at all, so the model-facing text is
    // the only thing left to show. That is the normal path for every denial.
    let completed = map_frame(
        &tool_frame(
            json!({
                "type": "tool/result",
                "data": { "message": {
                    "source": { "kind": "tool", "callId": "call_4" },
                    "content": [{ "type": "tool-result", "toolCallId": "call_4", "isError": true,
                                  "content": [{ "type": "text", "text": "Access to the path is denied" }] }],
                }},
            }),
            json!({ "for": "result" }),
        ),
        SESSION,
        &mut tools,
    );

    let Some(Event::ItemCompleted(Item::CommandExecution {
        aggregated_output,
        status,
        ..
    })) = completed.first()
    else {
        panic!("the failure should still complete the row, got {completed:?}");
    };

    assert_eq!(status.as_deref(), Some("failed"));
    assert!(
        aggregated_output
            .as_deref()
            .unwrap_or_default()
            .contains("denied")
    );
}

#[test]
fn a_result_for_a_call_this_session_never_saw_is_ignored() {
    // Attaching to a session mid-turn delivers results whose calls arrived
    // before the stream was open; inventing a row for them would show a
    // command row with no command in it.
    let completed = map_frame(
        &tool_frame(
            json!({
                "type": "tool/result",
                "data": { "message": {
                    "source": { "kind": "tool", "callId": "call-never-seen" },
                    "content": [{ "type": "tool-result", "toolCallId": "call-never-seen",
                                  "content": [{ "type": "text", "text": "x" }] }],
                }},
            }),
            json!({ "for": "result", "view": { "card": "terminal", "output": "x" }}),
        ),
        SESSION,
        &mut EventTracker::default(),
    );

    assert_eq!(completed, Vec::new());
}

#[test]
fn a_pending_inbox_snapshot_becomes_the_queued_prompt_rows() {
    use crate::chat::QueuedPrompt;
    use crate::dsh::session::queued_prompts;

    let items = json!([
        {
            "id": "msg-1",
            "placement": "queued",
            "message": { "content": [{ "type": "text", "text": "and then run the tests" }] },
        },
        {
            "id": "msg-2",
            "placement": "steering",
            "message": { "content": [
                { "type": "text", "text": "wait, " },
                { "type": "text", "text": "stop at the parser" },
            ]},
        },
        // Injected for the model, not queued by the user; listing it
        // would describe pending work nobody asked for.
        {
            "id": "msg-3",
            "placement": "context",
            "message": { "content": [{ "type": "text", "text": "goal changed" }] },
        },
    ]);

    assert_eq!(
        queued_prompts(&items),
        vec![
            QueuedPrompt {
                id: Some("msg-1".into()),
                text: "and then run the tests".into(),
            },
            QueuedPrompt {
                id: Some("msg-2".into()),
                text: "wait, stop at the parser".into(),
            },
        ]
    );
}

#[test]
fn task_projection_restores_replaces_and_clears_without_older_pages_winning() {
    use crate::dsh::projections::ProjectionTracker;
    use crate::progress::TaskStatus;

    let mut tracker = ProjectionTracker::default();

    let values = json!({"todos": [
        {"content": "Build editor", "status": "completed"},
        {"content": "Build insights", "status": "in_progress"}
    ]});

    let events = tracker.apply_baseline(&values, Some(5));

    let [Event::TaskListUpdated(tasks)] = events.as_slice() else {
        panic!("expected tasks")
    };

    assert_eq!(tasks.tally(), Some((1, 2)));
    assert_eq!(tasks.items[1].status, TaskStatus::InProgress);

    let cleared = tracker.apply_baseline(&json!({"todos": null}), Some(7));

    let [Event::TaskListUpdated(tasks)] = cleared.as_slice() else {
        panic!("expected cleared tasks")
    };

    assert!(tasks.items.is_empty());
    assert!(tracker.apply_baseline(&values, Some(6)).is_empty());
}

#[test]
fn a_pending_plan_selection_reports_the_state_it_is_heading_for() {
    use crate::dsh::projections::ProjectionTracker;

    let mut projections = ProjectionTracker::default();

    // Recorded and settled: the logged state is the answer.
    assert_eq!(
        projections.apply(
            &projection_frame("plan", json!({ "active": true, "pending": false })),
            SESSION,
        ),
        Some(vec![Event::PlanModeUpdated(true)]),
    );

    // A `/plan off` the host admitted but has not yet recorded. Reporting the
    // logged state here would leave the chip up after the user turned it off.
    assert_eq!(
        projections.apply(
            &projection_frame("plan", json!({ "active": true, "pending": true })),
            SESSION,
        ),
        Some(vec![Event::PlanModeUpdated(false)]),
    );
}

#[test]
fn the_session_stats_projection_reports_whole_log_counters() {
    use crate::dsh::projections::ProjectionTracker;

    let mut projections = ProjectionTracker::default();

    let events = projections
        .apply(
            &projection_frame(
                "sessionStats",
                json!({
                    "turns": 7,
                    "steps": 23,
                    "llmMs": 61_000,
                    "toolMs": 4_500,
                    "ttftMs": 900,
                    "ttftSteps": 23,
                    "decodeMs": 55_000,
                    "decodeTokens": 12_000,
                }),
            ),
            SESSION,
        )
        .expect("a projection frame for this session should be claimed");

    let [Event::SessionStatsUpdated(stats)] = events.as_slice() else {
        panic!("expected one stats snapshot, got {events:?}");
    };

    assert_eq!(stats.turns, 7);
    assert_eq!(stats.steps, 23);
    assert_eq!(stats.model_ms, 61_000);
    assert_eq!(stats.tool_ms, 4_500);
    assert_eq!(stats.decode_tokens, 12_000);
    assert_eq!(stats.decode_ms, 55_000);
}

#[test]
fn a_broken_preset_is_listed_by_the_harness_but_not_offered_for_selection() {
    use crate::dsh::catalogs::preset_catalog;

    let presets = preset_catalog(&json!([
        { "id": "coding", "name": "Coding", "description": "Ships code", "trust": "system" },
        { "id": "research", "trust": "system" },
        { "id": "mine", "description": "Mine", "trust": "user" },
        { "id": "plain", "trust": "user" },
        { "id": "half-written", "trust": "user", "broken": "names a plugin that will not load" },
    ]));

    let ids: Vec<&str> = presets.iter().map(|preset| preset.value.as_str()).collect();

    assert_eq!(ids, ["coding", "research", "mine", "plain"]);

    assert_eq!(presets[0].label, "Coding");
    assert_eq!(presets[0].description.as_deref(), Some("Ships code"));

    // A preset that published no name is still addressed by its id, so the id
    // is what the row shows instead of a blank label.
    assert_eq!(presets[1].label, "research");
    assert_eq!(presets[1].description, None);

    // A locally authored preset is as privileged as the plugins it names, and a
    // row that read like a shipped one would imply it had been vetted.
    assert_eq!(
        presets[2].description.as_deref(),
        Some("Locally authored — Mine")
    );
    assert_eq!(presets[3].description.as_deref(), Some("Locally authored"));
}

#[test]
fn branch_points_pair_each_prompt_with_the_seq_of_the_one_ahead_of_it() {
    use crate::chat::{ForkAnchor, ForkCheckpoint};
    use crate::dsh::history;

    let entry = |event: Value| json!({ "event": event });

    let prompt = |seq: u64, time: u64, text: &str| {
        entry(json!({
            "type": "user/message",
            "seq": seq,
            "time": time,
            "data": {
                "content": [{ "type": "text", "text": text }],
                "source": { "kind": "user" },
            },
        }))
    };

    let page = json!({
        "hasMore": false,
        "records": [
            entry(json!({ "type": "turn/start", "seq": 0, "data": { "turn": 1 } })),
            prompt(1, 1_770_000_000_000u64, "first"),
            entry(json!({ "type": "turn/end", "seq": 2, "data": { "turn": 1 } })),
            entry(json!({ "type": "turn/start", "seq": 3, "data": { "turn": 2 } })),
            prompt(4, 1_770_000_100_000u64, "second"),
            // A message the harness logged on the session's behalf is not
            // something the person asked, so it is not a branch point.
            entry(json!({
                "type": "user/message",
                "seq": 5,
                "time": 1_770_000_101_000u64,
                "data": {
                    "content": [{ "type": "text", "text": "injected" }],
                    "source": { "kind": "system" },
                },
            })),
            entry(json!({ "type": "turn/end", "seq": 6, "data": { "turn": 2 } })),
            entry(json!({ "type": "turn/start", "seq": 7, "data": { "turn": 3 } })),
            prompt(8, 1_770_000_200_000u64, "third"),
            entry(json!({ "type": "turn/end", "seq": 9, "data": { "turn": 3 } })),
        ],
    });

    // Newest first. Each anchor lies inside the turn ahead of the prompt it is
    // paired with, which is the turn the harness extends the cut through. The
    // oldest prompt has none ahead of it and is left out.
    assert_eq!(
        history::fork_checkpoints(&page),
        vec![
            ForkCheckpoint {
                prompt: "third".into(),
                timestamp: Some("2026-02-02T02:43:20Z".into()),
                anchor: ForkAnchor::DeepSeekThrough(4),
            },
            ForkCheckpoint {
                prompt: "second".into(),
                timestamp: Some("2026-02-02T02:41:40Z".into()),
                anchor: ForkAnchor::DeepSeekThrough(1),
            },
        ]
    );
}

#[test]
fn a_new_conversation_is_composed_from_the_remembered_preset() {
    let (base, requests, server) = scripted_api_server(vec![
        r#"{"result":{"ok":true,"value":{"sessionId":"sess_1","agentPreset":"reviewer"}}}"#,
    ]);

    let client = nmt_platform::runtime()
        .block_on(ApiClient::new(base))
        .expect("create API client");

    let (opened, refusal) = nmt_platform::runtime()
        .block_on(open_new_conversation(
            &client,
            Some(r"C:\Work\api"),
            Some("reviewer"),
        ))
        .expect("the conversation must open");

    server.join().expect("API server should exit");

    assert_eq!(
        requests.recv().unwrap()["payload"]["args"]["request"],
        json!({"cwd": r"C:\Work\api", "agentPreset": "reviewer"})
    );
    assert_eq!(opened.session_id, "sess_1");
    assert_eq!(opened.agent_preset.as_deref(), Some("reviewer"));
    assert!(refusal.is_none());
}

#[test]
fn a_refused_remembered_preset_opens_the_conversation_on_the_default() {
    let (base, requests, server) = scripted_api_server(vec![
        r#"{"result":{"ok":false,"error":{"code":"agent-presets/not-found","message":"no preset reviewer"}}}"#,
        r#"{"result":{"ok":true,"value":{"sessionId":"sess_1","agentPreset":"default"}}}"#,
    ]);

    let client = nmt_platform::runtime()
        .block_on(ApiClient::new(base))
        .expect("create API client");

    let (opened, refusal) = nmt_platform::runtime()
        .block_on(open_new_conversation(
            &client,
            Some(r"C:\Work\api"),
            Some("reviewer"),
        ))
        .expect("a refused preset must not keep the conversation from opening");

    server.join().expect("API server should exit");

    let sent: Vec<_> = requests
        .try_iter()
        .map(|request| request["payload"]["args"]["request"].clone())
        .collect();

    assert_eq!(
        sent,
        [
            json!({"cwd": r"C:\Work\api", "agentPreset": "reviewer"}),
            json!({"cwd": r"C:\Work\api"}),
        ]
    );
    assert_eq!(opened.agent_preset.as_deref(), Some("default"));
    assert!(refusal.is_some_and(|refusal| refusal.contains("no preset reviewer")));
}

#[test]
fn recent_conversations_are_filtered_by_the_primary_directory() {
    let listed = json!({
        "items": [
            {"sessionId": "in-primary", "cwd": r"C:\Work\api", "updatedAt": 2},
            {"sessionId": "in-additional", "cwd": r"C:\Work\web", "updatedAt": 3},
        ]
    });

    let rows = sessions(&listed, Some(r"C:\Work\api"));
    let ids: Vec<_> = rows.iter().map(|row| row.id.as_str()).collect();

    assert_eq!(ids, ["in-primary"]);
}

/// A bridge frame that drops or renames a field this side requires must be
/// refused as a whole (and logged) instead of decoding into empty-looking
/// events; a well-formed frame keeps decoding as before.
#[test]
fn a_frame_missing_a_required_field_is_dropped_not_emptied() {
    use crate::dsh::session::{
        fork_checkpoint_events, history_events, search_events, workflow_transcript_events,
    };

    // Renamed identity field: the whole frame is refused.
    let renamed = json!({
        "type": "nmt/workflow-transcript", "sessionId": SESSION, "task": "t1", "agentId": "a1",
    });

    assert_eq!(workflow_transcript_events(&renamed, SESSION), Vec::new());

    let complete = json!({
        "type": "nmt/workflow-transcript", "sessionId": SESSION, "taskId": "t1", "agentId": "a1",
        "page": { "records": [] },
    });

    // An answer for the conversation this tab left is dropped.
    assert_eq!(
        workflow_transcript_events(&complete, "left-behind"),
        Vec::new()
    );

    assert!(matches!(
        complete_events(&complete).as_slice(),
        [Event::WorkflowAgentTranscript { task_id, agent_id, .. }]
            if task_id == "t1" && agent_id == "a1"
    ));

    fn complete_events(payload: &Value) -> Vec<Event> {
        workflow_transcript_events(payload, SESSION)
    }

    // A search error whose value is not a string is drift, never a success.
    let drifted = json!({ "type": "nmt/search", "error": 500 });

    assert_eq!(search_events(&drifted), Vec::new());

    let failed = json!({ "type": "nmt/search", "error": "index unavailable" });

    assert!(matches!(
        search_events(&failed).as_slice(),
        [Event::Error { fatal: false, .. }]
    ));

    // All-optional frames still decode when fields are absent.
    assert!(matches!(
        history_events(&json!({ "type": "nmt/history" })).as_slice(),
        [Event::History(sessions)] if sessions.is_empty()
    ));
    assert!(matches!(
        fork_checkpoint_events(
            &json!({ "type": "nmt/fork-checkpoints", "sessionId": SESSION }),
            SESSION
        )
        .as_slice(),
        [Event::ForkCheckpoints(Ok(checkpoints))] if checkpoints.is_empty()
    ));
    assert_eq!(
        fork_checkpoint_events(
            &json!({ "type": "nmt/fork-checkpoints", "sessionId": SESSION }),
            "left-behind"
        ),
        Vec::new()
    );
}

#[test]
fn raw_tool_events_keep_commands_output_and_applied_diffs() {
    let mut tools = EventTracker::default();

    let call = json!({"type":"tool/call","data":{
        "callId":"raw-shell","name":"bash","arguments":"{\"command\":\"echo ok\",\"description\":\"Print a marker\"}"
    }});

    let started = mapping::map_session_event(&call, &Value::Null, &mut tools);

    assert!(
        matches!(&started[0], Event::ItemStarted(Item::CommandExecution { command, .. }) if command == "echo ok")
    );

    let result = json!({"type":"tool/result","data":{"message":{
        "source":{"callId":"raw-shell"},"content":[{"type":"tool-result","content":[{"type":"text","text":"ok\n"}]}]
    }}});

    let completed = mapping::map_session_event(&result, &Value::Null, &mut tools);

    assert!(
        matches!(&completed[0], Event::ItemCompleted(Item::CommandExecution { aggregated_output: Some(output), .. }) if output == "ok\n")
    );

    for (output, expected) in [
        ("ok\n", Some(0)),
        ("error\n[exit code: 17]\n", Some(17)),
        ("[timed out after 500ms]", None),
    ] {
        mapping::map_session_event(&call, &Value::Null, &mut tools);

        let mut result = result.clone();

        result["data"]["message"]["content"][0]["content"][0]["text"] = json!(output);

        let completed = mapping::map_session_event(&result, &Value::Null, &mut tools);

        assert!(
            matches!(&completed[0], Event::ItemCompleted(Item::CommandExecution { exit_code, .. }) if *exit_code == expected)
        );
    }

    let call = json!({"type":"tool/call","data":{
        "callId":"raw-edit","name":"edit","arguments":"{\"file_path\":\"a.txt\",\"old_string\":\"before\",\"new_string\":\"after\"}"
    }});

    mapping::map_session_event(&call, &Value::Null, &mut tools);

    let result = json!({"type":"tool/result","data":{
        "meta":{"diffs":[{"path":"a.txt","oldText":"context\nbefore","newText":"context\nafter"}]},
        "message":{"source":{"callId":"raw-edit"},"content":[{"content":[]}]}
    }});

    let completed = mapping::map_session_event(&result, &Value::Null, &mut tools);

    assert!(
        matches!(&completed[0], Event::ItemCompleted(Item::FileChange { paths, diff: Some(diff), .. }) if paths == "a.txt" && diff.contains("+after") && diff.contains("context"))
    );
}

#[test]
fn stopped_history_keeps_packed_and_unpacked_partial_text() {
    let page = json!({"records":[
        {"event":{"type":"turn/start","time":1000,"data":{}}},
        {"event":{"type":"assistant/chunk","data":{"turn":1,"step":1,"chunk":{"type":"block-start","blockType":"text","index":0}}}},
        {"type":"chunks","event":{"type":"chunkrow/text-chunks","data":{"turn":1,"step":1,"index":0,"texts":["hello"," "]}}},
        {"event":{"type":"assistant/chunk","data":{"turn":1,"step":1,"chunk":{"type":"text-delta","index":0,"text":"world"}}}},
        {"event":{"type":"turn/end","time":4000,"data":{"reason":{"kind":"aborted"}}}}
    ]});

    let turns = history::replay(&page);

    assert_eq!(turns.len(), 1);
    assert!(turns[0].interrupted);
    assert!(
        matches!(&turns[0].items[0].item, Item::AgentMessage { text: Some(text), .. } if text == "hello world")
    );
}

#[test]
fn an_older_log_snapshot_cannot_overwrite_a_newer_control_update() {
    use crate::dsh::projections::ProjectionTracker;

    let mut tracker = ProjectionTracker::default();

    let frame = json!({"payload":{"type":"session/projection","sessionId":SESSION,"key":"title","seq":12,"value":"new title"}});

    assert_eq!(
        tracker.apply(&frame, SESSION),
        Some(vec![Event::TitleUpdated("new title".into())])
    );
    assert!(
        tracker
            .apply_baseline(&json!({"title":"old title"}), Some(10))
            .is_empty()
    );
    assert_eq!(
        tracker.apply_baseline(&json!({"title":"latest title"}), Some(13)),
        vec![Event::TitleUpdated("latest title".into())]
    );
}
