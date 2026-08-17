//! Frame payloads here are trimmed copies of ones a real host emitted, so a
//! mapping that passes these matches what the harness actually sends rather
//! than what its declarations suggest.

use serde_json::{Value, json};

use crate::chat::{Event, Item};
use crate::deepseek::mapping::{ToolTracker, map_frame};

const SESSION: &str = "session-debb6efc";

fn session_frame(event: Value) -> Value {
    json!({
        "type": "server-request",
        "rpcId": "frame-1",
        "method": "session/event",
        "payload": { "type": "session/event", "sessionId": SESSION, "event": event },
    })
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
        &mut ToolTracker::default(),
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
        &mut ToolTracker::default(),
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
        &mut ToolTracker::default(),
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
    // The completed message carries its blocks in the same order the chunks
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
        &mut ToolTracker::default(),
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
        map_frame(&session_frame(prompt), SESSION, &mut ToolTracker::default()),
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
            map_frame(&frame, SESSION, &mut ToolTracker::default()),
            Vec::new(),
            "{injected}"
        );
    }
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
            &mut ToolTracker::default()
        ),
        vec![Event::TurnCompleted { error: None }]
    );

    let completed = json!({
        "type": "turn/end",
        "data": { "turn": 1, "reason": { "kind": "completed" } },
    });
    assert_eq!(
        map_frame(
            &session_frame(completed),
            SESSION,
            &mut ToolTracker::default()
        ),
        vec![Event::TurnCompleted { error: None }]
    );

    let failed = json!({
        "type": "turn/end",
        "data": { "turn": 1, "reason": { "kind": "failed", "message": "NO_ADAPTER" } },
    });
    assert_eq!(
        map_frame(&session_frame(failed), SESSION, &mut ToolTracker::default()),
        vec![Event::TurnCompleted {
            error: Some("NO_ADAPTER".into()),
        }]
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
        map_frame(&frame, "session-someone-else", &mut ToolTracker::default()),
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
        map_frame(&unknown_event, SESSION, &mut ToolTracker::default()),
        Vec::new()
    );

    let unknown_frame = json!({
        "type": "server-request",
        "payload": { "type": "session/telepathy", "sessionId": SESSION },
    });
    assert_eq!(
        map_frame(&unknown_frame, SESSION, &mut ToolTracker::default()),
        Vec::new()
    );

    // A chunk kind that is not one of the two that stream text.
    let unknown_chunk = session_frame(chunk(
        json!({ "type": "signature-delta", "index": 0, "text": "x" }),
    ));
    assert_eq!(
        map_frame(&unknown_chunk, SESSION, &mut ToolTracker::default()),
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
        map_frame(&agent_error, SESSION, &mut ToolTracker::default()),
        vec![Event::ItemStarted(Item::Error {
            text: "the model provider refused the request".into(),
        })]
    );

    // An agent error for another tab's session belongs to that tab.
    assert_eq!(
        map_frame(&agent_error, "session-other", &mut ToolTracker::default()),
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
        map_frame(&stream_error, SESSION, &mut ToolTracker::default()),
        vec![Event::ItemStarted(Item::Error {
            text: "the stream ended".into(),
        })]
    );
}

#[test]
fn the_tested_release_is_inside_the_supported_range() {
    use semver::Version;

    use crate::deepseek::version::{VersionSupport, classify};

    // A pre-release only satisfies a requirement when some comparator carries
    // the same triple and its own pre-release, which is why the lower bound is
    // written as a pre-release rather than as a plain `0.1.0`.
    for inside in ["0.1.0-rc.6", "0.1.0", "0.1.4"] {
        assert_eq!(
            classify(&Version::parse(inside).unwrap()),
            VersionSupport::Supported,
            "{inside}"
        );
    }

    for outside in ["0.1.0-rc.5", "0.2.0", "1.0.0"] {
        assert!(
            matches!(
                classify(&Version::parse(outside).unwrap()),
                VersionSupport::Unsupported { .. }
            ),
            "{outside}"
        );
    }
}

#[test]
fn an_unresolvable_harness_is_reported_as_missing_rather_than_as_a_failed_start() {
    use crate::LaunchConfig;
    use crate::deepseek::{HostError, Session};

    // The two failures have different answers for the user, so the adapter has
    // to tell them apart before any process is spawned.
    let launch = LaunchConfig {
        executable: "dsh-that-is-not-installed".to_string(),
        ..LaunchConfig::default()
    };

    assert!(matches!(
        Session::create(&launch, None, |_| {}),
        Err(HostError::NotInstalled(_))
    ));
}

#[test]
fn an_approval_request_carries_what_answering_it_needs() {
    use crate::deepseek::mapping::approval_request;

    // Shape copied from a real blocked turn: the harness waits here, so a
    // client that cannot recognize this leaves the agent stalled with no
    // visible reason.
    let frame = json!({
        "type": "server-request",
        "rpcId": "3fcb9bcf-614d-414e-9041-ada82f9a0fad",
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
    assert_eq!(request.rpc_id, "3fcb9bcf-614d-414e-9041-ada82f9a0fad");
    assert_eq!(request.approval_id, "5cc446e3-8026-44f7-9fc5-e62d5213d18a");
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
fn a_workflow_run_is_folded_from_its_own_increments() {
    use crate::deepseek::workflows::WorkflowTracker;
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
fn the_child_catalog_becomes_rows_that_can_be_opened() {
    use crate::background_task::{BackgroundTaskRefs, BackgroundTaskState};
    use crate::deepseek::subagents;

    let catalog = json!({
        "parentAvailable": true,
        "entries": [
            {
                "kind": "child",
                "id": "child-1",
                "activity": "running",
                "hasChildren": false,
                "mode": "continuable",
                "label": "Review the diff",
            },
            {
                "kind": "child",
                "id": "child-2",
                "activity": "inactive",
                "hasChildren": false,
                "mode": "one-shot",
            },
            // Names a child the harness could not read, so nothing about it can
            // be opened and a row would only report its own unreadability.
            { "kind": "diagnostic", "id": "child-3", "reason": "corrupt" },
        ],
    });

    let snapshot = subagents::snapshot(&catalog, SESSION, 7);
    assert_eq!(snapshot.tasks.len(), 2);
    assert_eq!(snapshot.parent_session.id, SESSION);

    let first = &snapshot.tasks[0];
    assert_eq!(first.key.id, "child-1");
    assert_eq!(first.display_name.as_deref(), Some("Review the diff"));
    assert_eq!(first.state, BackgroundTaskState::Working);
    // Only a running continuable child has anything a stop can reach.
    assert!(first.can_stop);
    assert!(!snapshot.tasks[1].can_stop);
    assert_eq!(snapshot.tasks[1].state, BackgroundTaskState::Done);

    // The pair is what addresses a child's conversation, so the row carries the
    // parent as well as which of the two child kinds it is.
    assert_eq!(
        first.refs,
        BackgroundTaskRefs::DeepSeek {
            parent_session_id: SESSION.to_string(),
            continuable: true,
        }
    );
}

#[test]
fn the_command_registry_fills_the_palette() {
    use crate::chat::{SlashCommandArguments, SlashCommandRunPolicy, SlashCommandSource};
    use crate::deepseek::commands;

    let listed = json!([
        { "name": "compact", "description": "Summarize the conversation so far" },
        {
            "name": "permission",
            "description": "Switch the permission preset",
            "input": { "hint": "preset name" },
        },
    ]);

    let catalog = commands::catalog(&listed);
    assert_eq!(catalog.len(), 2);
    assert_eq!(catalog[0].name, "compact");
    assert_eq!(catalog[0].source, SlashCommandSource::Provider);
    // The registry settles a command itself rather than handing it to the
    // model, so none of them wait for a turn.
    assert_eq!(catalog[0].run_policy, SlashCommandRunPolicy::Immediate);
    // An input hint is what says the name is followed by free text.
    assert_eq!(catalog[0].arguments, SlashCommandArguments::None);
    assert_eq!(catalog[1].arguments, SlashCommandArguments::Freeform);
    assert_eq!(catalog[1].argument_hint.as_deref(), Some("preset name"));

    // The registry resolves the agent from a session id, and the argument is
    // named by that resolver rather than by the method's own parameter.
    assert_eq!(
        commands::agent_args(SESSION),
        json!({ "args": { "agentId": SESSION } })
    );
}

#[test]
fn the_session_list_offers_only_what_this_tab_can_continue() {
    use std::time::{Duration, UNIX_EPOCH};

    use crate::deepseek::history;

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
            // Belongs to a parent conversation rather than to this list.
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
fn a_replayed_page_rebuilds_turns_from_the_same_events_the_stream_carries() {
    use crate::deepseek::history;

    let entry = |event: Value| json!({ "event": event });
    let page = json!({
        "hasMore": false,
        "events": [
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
        map_frame(&start, SESSION, &mut ToolTracker::default()),
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
        map_frame(&summary, SESSION, &mut ToolTracker::default()),
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
        map_frame(&end, SESSION, &mut ToolTracker::default()),
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
        map_frame(&replacement, SESSION, &mut ToolTracker::default()),
        Vec::new()
    );
}

#[test]
fn a_todo_write_renders_as_the_shared_checklist_shape() {
    let frame = session_frame(json!({
        "type": "todo/write",
        "seq": 88,
        "data": {
            "todos": [
                { "content": "read the spec", "status": "completed" },
                { "content": "write the mapping", "status": "in_progress" },
                { "content": "cover it", "status": "pending" },
            ],
        },
    }));

    let events = map_frame(&frame, SESSION, &mut ToolTracker::default());
    let [Event::ItemCompleted(item)] = events.as_slice() else {
        panic!("expected one todo row, got {events:?}");
    };
    // The tally the transcript shows reads this shape, so the row has to speak
    // it rather than a second vocabulary of its own.
    assert_eq!(item.task_tally(), Some((1, 3)));

    let Item::Other { id, kind, .. } = item else {
        panic!("expected a generic row, got {item:?}");
    };
    assert_eq!(kind, "TodoWrite");
    // Each write describes its own moment, so rows do not collapse into one.
    assert_eq!(id, "todo:88");

    let empty = session_frame(json!({
        "type": "todo/write",
        "seq": 89,
        "data": { "todos": [] },
    }));
    assert_eq!(
        map_frame(&empty, SESSION, &mut ToolTracker::default()),
        Vec::new()
    );
}

#[test]
fn the_model_directory_addresses_a_pick_as_a_provider_and_model_pair() {
    use crate::deepseek::models::ModelDirectory;

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
        Some(("openrouter", "deepseek-chat"))
    );
    assert_eq!(directory.route("nothing-like-this"), None);
}

#[test]
fn a_selection_outside_the_catalog_still_shows_in_the_picker() {
    use crate::deepseek::models::ModelDirectory;

    // Catalog membership is advisory: a route can serve a model it stopped
    // advertising, and that session runs perfectly well.
    let directory = ModelDirectory::parse(&json!({
        "current": { "provider": "deepseek", "model": "deepseek-retired" },
        "routable": true,
        "groups": [{
            "id": "deepseek",
            "name": "DeepSeek",
            "models": [{ "id": "deepseek-chat", "name": "DeepSeek Chat" }],
        }],
        "failures": [],
    }));

    assert_eq!(directory.selected(), Some("deepseek-retired"));
    assert_eq!(
        directory.route("deepseek-retired"),
        Some(("deepseek", "deepseek-retired"))
    );
    assert_eq!(directory.effort(), None);
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
    use crate::deepseek::projections::ProjectionTracker;

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

    // Another tab's projection is not this tab's accounting.
    assert_eq!(
        usage.apply(&projection_frame("tokenUsage", json!({})), "session-other"),
        None
    );
}

#[test]
fn the_permission_presets_come_from_the_deployment_rather_than_from_here() {
    use crate::deepseek::projections::ProjectionTracker;

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
}

#[test]
fn the_history_page_baseline_seeds_what_a_live_push_would_not() {
    use crate::deepseek::projections::ProjectionTracker;

    // A push reports only what changed after the tab attached, so a session
    // that has been running since before it opened would show nothing.
    let mut projections = ProjectionTracker::default();
    let events = projections.apply_baseline(&json!({
        "contextPressure": { "projectedTokens": 4200, "contextWindow": 64000 },
        "title": "Map the harness",
    }));

    let [Event::ContextWindowUpdated(window)] = events.as_slice() else {
        panic!("expected one window snapshot, got {events:?}");
    };
    assert_eq!(window.current.total_tokens, 4200);
    assert_eq!(window.max_tokens, Some(64_000));
}

#[test]
fn a_context_breakdown_becomes_the_composition_segments() {
    use crate::deepseek::projections::ProjectionTracker;

    let mut usage = ProjectionTracker::default();
    usage.apply(
        &projection_frame(
            "contextPressure",
            json!({ "projectedTokens": 900, "contextWindow": 64000 }),
        ),
        SESSION,
    );

    let events = usage
        .apply(
            &projection_frame(
                "contextBreakdown",
                json!({ "systemTokens": 400, "toolsTokens": 250, "messageTokens": 1000 }),
            ),
            SESSION,
        )
        .expect("a projection frame for this session should be claimed");

    let [Event::ContextCompositionUpdated(composition)] = events.as_slice() else {
        panic!("expected one composition, got {events:?}");
    };
    assert_eq!(composition.segments.len(), 3);
    assert_eq!(composition.segments[1].label, "Tools");
    assert_eq!(composition.segments[1].tokens, 250);
    // The three figures share one estimator, so their sum is the only total
    // that describes this split.
    assert_eq!(composition.used_tokens, 1650);
    assert_eq!(composition.max_tokens, Some(64_000));
}

#[test]
fn a_question_request_carries_the_ids_an_answer_is_matched_against() {
    use crate::deepseek::mapping::question_request;

    let frame = json!({
        "type": "server-request",
        "rpcId": "0f21a6f2-7f52-4b0f-bb2f-9c0e9d2f0a11",
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
    assert_eq!(request.rpc_id, "0f21a6f2-7f52-4b0f-bb2f-9c0e9d2f0a11");
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

    // Another tab's question belongs to that tab.
    assert_eq!(question_request(&frame, "session-other"), None);
}

#[test]
fn a_resolved_question_takes_the_card_down() {
    let frame = json!({
        "type": "server-request",
        "payload": {
            "type": "question/resolved",
            "sessionId": SESSION,
            "questionRpcId": "0f21a6f2",
            "outcome": "answered",
        },
    });

    assert_eq!(
        map_frame(&frame, SESSION, &mut ToolTracker::default()),
        vec![Event::QuestionsResolved]
    );
    assert_eq!(
        map_frame(&frame, "session-other", &mut ToolTracker::default()),
        Vec::new()
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
        map_frame(&frame, SESSION, &mut ToolTracker::default()),
        vec![Event::ApprovalResolved]
    );
    assert_eq!(
        map_frame(&frame, "session-other", &mut ToolTracker::default()),
        Vec::new()
    );
}

/// A tool frame carries the host-computed card alongside the logged event.
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
    let mut tools = ToolTracker::default();

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
            purpose: None,
            aggregated_output: Some("hello\n".into()),
            status: Some("completed".into()),
            exit_code: Some(0),
        })]
    );
}

#[test]
fn an_edit_becomes_a_file_row_whose_result_diff_carries_context() {
    let mut tools = ToolTracker::default();

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
    // The result diff carries surrounding lines the arguments never had.
    let body = diff.as_deref().unwrap_or_default();
    assert!(body.contains("-line one"), "{body}");
    assert!(body.contains("+after"), "{body}");
    assert_eq!(status.as_deref(), Some("completed"));
}

#[test]
fn a_card_this_build_does_not_model_still_shows_the_call() {
    let mut tools = ToolTracker::default();

    // read, search, and web cards all land here, as does any card a later
    // harness release adds. None of them may vanish from the transcript.
    let started = map_frame(
        &tool_frame(
            json!({
                "type": "tool/call",
                "data": { "callId": "call_3", "name": "read" },
            }),
            json!({ "for": "call", "view": {
                "card": "generic",
                "title": "Read probe-target.txt",
                "kind": "read",
            }}),
        ),
        SESSION,
        &mut tools,
    );
    assert_eq!(
        started,
        vec![Event::ItemStarted(Item::Other {
            id: "call_3".into(),
            kind: "read".into(),
            title: "Read probe-target.txt".into(),
            output: None,
            status: Some("inProgress".into()),
        })]
    );
}

#[test]
fn a_failed_call_reports_the_text_the_model_saw() {
    let mut tools = ToolTracker::default();

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
        &mut ToolTracker::default(),
    );

    assert_eq!(completed, Vec::new());
}
