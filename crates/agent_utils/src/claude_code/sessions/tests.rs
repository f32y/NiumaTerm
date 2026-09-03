use std::io::BufRead;

use crate::background_task::{BackgroundTaskState, BackgroundTaskTranscript};
use crate::chat::{CompactionTrigger, Item};
use crate::claude_code::sessions::*;

/// Replayed conversation with its turn grouping flattened away, for the tests
/// that assert what a session replays rather than how it is divided.
fn replayed_items(reader: impl BufRead) -> Vec<Item> {
    parse_replay(reader)
        .into_iter()
        .flat_map(|turn| turn.items)
        .map(|entry| entry.item)
        .collect()
}

const ACTIVE_CHAIN_FIXTURE: &str =
    include_str!("../../../tests/fixtures/claude/active-chain.jsonl");
const MISSING_PARENT_FIXTURE: &str =
    include_str!("../../../tests/fixtures/claude/missing-parent.jsonl");

#[test]
fn active_chain_uses_the_latest_main_leaf_without_logical_parent_jumps() {
    let transcript = TranscriptIndex::read(ACTIVE_CHAIN_FIXTURE.as_bytes());
    let uuids = transcript
        .active_records()
        .filter_map(|record| record["uuid"].as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        uuids,
        vec![
            "00000000-0000-4000-8000-000000000001",
            "00000000-0000-4000-8000-000000000002",
            "00000000-0000-4000-8000-000000000003",
            "00000000-0000-4000-8000-000000000004",
            "00000000-0000-4000-8000-000000000007",
            "00000000-0000-4000-8000-000000000008",
            "00000000-0000-4000-8000-000000000009",
            "00000000-0000-4000-8000-000000000010",
            "00000000-0000-4000-8000-000000000011",
            "00000000-0000-4000-8000-000000000012",
        ]
    );
    assert!(!uuids.contains(&"00000000-0000-4000-8000-000000000005"));
    assert!(!uuids.contains(&"00000000-0000-4000-8000-000000000013"));
    assert!(!uuids.contains(&"00000000-0000-4000-8000-000000000014"));
}

#[test]
fn rewind_checkpoints_are_human_prompts_on_the_active_chain() {
    let transcript = TranscriptIndex::read(ACTIVE_CHAIN_FIXTURE.as_bytes());
    let checkpoints = transcript.checkpoints();

    assert_eq!(checkpoints.len(), 2);
    assert_eq!(checkpoints[0].prompt, "active prompt");
    assert_eq!(
        checkpoints[0].parent_message_id.as_deref(),
        Some("00000000-0000-4000-8000-000000000004")
    );
    assert_eq!(
        checkpoints[0].file_restore_availability,
        FileRestoreAvailability::Available
    );
    assert_eq!(checkpoints[1].prompt, "first prompt");
    assert_eq!(checkpoints[1].parent_message_id, None);
    assert_eq!(
        checkpoints[1].timestamp.as_deref(),
        Some("2026-08-07T01:00:00Z")
    );
    assert!(
        checkpoints
            .iter()
            .all(|checkpoint| checkpoint.prompt != "abandoned prompt")
    );
}

#[test]
fn forking_before_the_first_prompt_starts_a_fresh_session() {
    let transcript = TranscriptIndex::read(ACTIVE_CHAIN_FIXTURE.as_bytes());
    let fork = build_fork_records(
        &transcript,
        "10000000-0000-4000-8000-000000000000",
        "00000000-0000-4000-8000-000000000001",
        "30000000-0000-4000-8000-000000000000",
        "2026-08-07T02:00:00.000Z",
    )
    .unwrap();

    assert_eq!(fork, None);
}

#[test]
fn fork_remaps_the_exact_active_prefix_and_preserves_unknown_fields() {
    let next_prompt = serde_json::json!({
        "type": "user",
        "uuid": "00000000-0000-4000-8000-000000000015",
        "parentUuid": "00000000-0000-4000-8000-000000000012",
        "sessionId": "10000000-0000-4000-8000-000000000000",
        "timestamp": "2026-08-07T01:00:14Z",
        "message": {"role": "user", "content": "next prompt"}
    });
    let source = format!("{ACTIVE_CHAIN_FIXTURE}\n{next_prompt}\n");
    let transcript = TranscriptIndex::read(source.as_bytes());
    let new_session_id = "30000000-0000-4000-8000-000000000000";
    let records = build_fork_records(
        &transcript,
        "10000000-0000-4000-8000-000000000000",
        "00000000-0000-4000-8000-000000000015",
        new_session_id,
        "2026-08-07T02:00:00.000Z",
    )
    .unwrap()
    .unwrap();

    let transcript_records = records
        .iter()
        .filter(|record| is_transcript_entry(record))
        .collect::<Vec<_>>();
    assert_eq!(transcript_records.len(), 10);
    assert!(
        records
            .iter()
            .all(|record| record["type"].as_str() != Some("file-history-snapshot"))
    );
    assert!(
        transcript_records
            .iter()
            .all(|record| record["sessionId"].as_str() == Some(new_session_id))
    );
    assert!(
        transcript_records
            .iter()
            .any(|record| { record["unknownFutureField"]["preserve"].as_bool() == Some(true) })
    );

    let new_uuids = transcript_records
        .iter()
        .filter_map(|record| record["uuid"].as_str())
        .collect::<HashSet<_>>();
    assert_eq!(new_uuids.len(), transcript_records.len());
    assert!(new_uuids.iter().all(|uuid| !source.contains(*uuid)));
    for record in &transcript_records {
        if let Some(parent) = record["parentUuid"].as_str() {
            assert!(new_uuids.contains(parent));
        }
    }

    let serialized = records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let mut expected = replayed_items(ACTIVE_CHAIN_FIXTURE.as_bytes());
    let mut actual = replayed_items(serialized.as_bytes());
    for items in [&mut expected, &mut actual] {
        for item in items {
            if let Item::Compaction { id, .. } = item {
                *id = "normalized-compaction".into();
            }
        }
    }
    assert_eq!(actual, expected);
}

#[test]
fn fork_rejects_a_message_outside_the_active_chain() {
    let transcript = TranscriptIndex::read(ACTIVE_CHAIN_FIXTURE.as_bytes());
    let error = build_fork_records(
        &transcript,
        "10000000-0000-4000-8000-000000000000",
        "00000000-0000-4000-8000-000000000005",
        "30000000-0000-4000-8000-000000000000",
        "2026-08-07T02:00:00.000Z",
    )
    .unwrap_err();

    assert!(error.contains("not a prompt on the active conversation"));
}

#[test]
fn legacy_transcripts_without_file_snapshots_still_support_conversation_forks() {
    let legacy = ACTIVE_CHAIN_FIXTURE
        .lines()
        .filter(|line| !line.contains("\"type\":\"file-history-snapshot\""))
        .collect::<Vec<_>>()
        .join("\n");
    let transcript = TranscriptIndex::read(legacy.as_bytes());

    assert!(transcript.checkpoints().iter().all(|checkpoint| {
        checkpoint.file_restore_availability == FileRestoreAvailability::Unavailable
    }));
    assert!(
        build_fork_records(
            &transcript,
            "10000000-0000-4000-8000-000000000000",
            "00000000-0000-4000-8000-000000000007",
            "30000000-0000-4000-8000-000000000000",
            "2026-08-07T02:00:00.000Z",
        )
        .unwrap()
        .is_some()
    );
}

#[test]
fn atomic_fork_write_never_changes_the_source_file() {
    let test_dir = env::temp_dir().join(format!("niumaterm-fork-{}", Uuid::new_v4()));
    fs::create_dir(&test_dir).unwrap();
    let source_path = test_dir.join("source.jsonl");
    let source = b"source transcript remains immutable\n";
    fs::write(&source_path, source).unwrap();
    let session_id = Uuid::new_v4().to_string();
    let records = vec![serde_json::json!({"type": "custom-title"})];

    let target = write_fork_file(&test_dir, &session_id, &records).unwrap();

    assert_eq!(fs::read(&source_path).unwrap(), source);
    assert_eq!(
        fs::read_to_string(target).unwrap(),
        "{\"type\":\"custom-title\"}\n"
    );
    fs::remove_dir_all(test_dir).unwrap();
}

#[test]
fn active_chain_replay_keeps_tools_and_compaction_but_drops_abandoned_content() {
    let items = replayed_items(ACTIVE_CHAIN_FIXTURE.as_bytes());

    assert!(items.iter().any(|item| matches!(
        item,
        Item::Other {
            id,
            output: Some(output),
            status: Some(status),
            ..
        } if id == "tool-1" && output == "contents" && status == "completed"
    )));
    assert!(items.iter().any(|item| matches!(
        item,
        Item::CommandExecution {
            id,
            aggregated_output: Some(output),
            status: Some(status),
            ..
        } if id == "tool-2" && output == "ok" && status == "completed"
    )));
    assert!(items.iter().any(|item| matches!(
        item,
        Item::Compaction { detail, .. }
            if detail.summary.as_deref() == Some("summary text")
                && detail.pre_tokens == Some(1000)
                && detail.post_tokens == Some(100)
    )));

    let visible_text = items
        .iter()
        .filter_map(|item| match item {
            Item::UserMessage { text } | Item::AgentMessage { text, .. } => text.as_deref(),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(visible_text.contains(&"first prompt"));
    assert!(visible_text.contains(&"active prompt"));
    assert!(visible_text.contains(&"active answer"));
    assert!(!visible_text.contains(&"abandoned prompt"));
    assert!(!visible_text.contains(&"abandoned answer"));
    assert!(!visible_text.contains(&"sidechain answer"));
}

#[test]
fn a_missing_parent_keeps_only_the_reachable_suffix() {
    let transcript = TranscriptIndex::read(MISSING_PARENT_FIXTURE.as_bytes());
    let uuids = transcript
        .active_records()
        .filter_map(|record| record["uuid"].as_str())
        .collect::<Vec<_>>();

    assert_eq!(uuids, vec!["20000000-0000-4000-8000-000000000002"]);
    assert_eq!(
        transcript.broken_parent.as_deref(),
        Some("ffffffff-ffff-4fff-8fff-ffffffffffff")
    );
    assert_eq!(
        replayed_items(MISSING_PARENT_FIXTURE.as_bytes()),
        vec![Item::AgentMessage {
            id: "replay-message-0".into(),
            text: Some("reachable suffix only".into()),
        }]
    );
}

#[test]
fn cwd_munges_to_the_cli_project_directory_name() {
    assert_eq!(
        munge_cwd("C:\\Workspace\\NiumaTerm"),
        "C--Workspace-NiumaTerm"
    );
    assert_eq!(munge_cwd("/home/u/my.project"), "-home-u-my-project");
}

#[test]
fn titles_come_from_the_first_real_user_prompt() {
    // Sidechain, meta, and tool-result records are not prompts.
    assert_eq!(
        user_prompt_text(&serde_json::json!({"type": "user", "isSidechain": true,
                "message": {"content": [{"type": "text", "text": "sub"}]}})),
        None
    );
    assert_eq!(
        user_prompt_text(&serde_json::json!({"type": "user", "isMeta": true,
            "message": {"content": "caveat"}})),
        None
    );
    assert_eq!(
        user_prompt_text(&serde_json::json!({"type": "user",
            "message": {"content": [{"type": "tool_result", "tool_use_id": "t"}]}})),
        None
    );

    let record = serde_json::json!({"type": "user", "gitBranch": "dev",
        "message": {"content": [{"type": "text", "text": "fix the login bug\nmore detail"}]}});

    assert_eq!(
        user_prompt_text(&record).as_deref().and_then(title_line),
        Some("fix the login bug more detail".to_string())
    );
}

#[test]
fn a_compaction_summary_is_never_mistaken_for_a_prompt() {
    // The CLI stores it as a user turn, so without the guard it would both
    // title the session and replay as something the user typed.
    let summary = serde_json::json!({"type": "user", "isCompactSummary": true,
        "message": {"content": "This session is being continued from a previous…"}});

    assert_eq!(user_prompt_text(&summary), None);
    assert_eq!(
        compaction_summary_text(&summary).as_deref(),
        Some("This session is being continued from a previous…")
    );
    assert_eq!(
        compaction_summary_text(&serde_json::json!({"type": "user",
            "message": {"content": "a real prompt"}})),
        None
    );
}

/// The ordering current CLI builds write: the boundary marker opens the
/// compacted chain with no parent, and the summary turn hangs off it.
#[test]
fn a_compaction_replays_as_one_row_carrying_summary_and_accounting() {
    let lines = [
        serde_json::json!({"type": "system", "subtype": "compact_boundary",
            "uuid": "boundary-uuid", "parentUuid": null, "isMeta": false,
            "content": "Conversation compacted",
            "compactMetadata": {"trigger": "auto", "preTokens": 154_000,
                "postTokens": 32_000, "messagesSummarized": 87}}),
        serde_json::json!({"type": "user", "uuid": "summary-uuid",
            "parentUuid": "boundary-uuid",
            "isCompactSummary": true, "isVisibleInTranscriptOnly": true,
            "message": {"content": "## Summary\nwhat happened so far"}}),
        serde_json::json!({"type": "assistant", "uuid": "answer-uuid",
            "parentUuid": "summary-uuid",
            "message": {"content": [{"type": "text", "text": "answer"}]}}),
    ];
    let content: Vec<String> = lines.iter().map(|l| l.to_string()).collect();

    let items = replayed_items(content.join("\n").as_bytes());

    assert_eq!(
        items,
        vec![
            Item::Compaction {
                // The boundary record's identity wins, so the same
                // compaction cannot also arrive live as a second row.
                id: "compaction-boundary-uuid".into(),
                detail: Compaction {
                    trigger: Some(CompactionTrigger::Automatic),
                    pre_tokens: Some(154_000),
                    post_tokens: Some(32_000),
                    messages_summarized: Some(87),
                    user_context: None,
                    summary: Some("## Summary\nwhat happened so far".into()),
                },
            },
            Item::AgentMessage {
                id: "replay-message-0".into(),
                text: Some("answer".into())
            },
        ]
    );
}

/// The ordering older CLI builds wrote, where the summary turn came first and
/// the boundary marker hung off it.
#[test]
fn a_summary_before_its_boundary_marker_still_replays_as_one_row() {
    let lines = [
        serde_json::json!({"type": "user", "uuid": "question-uuid", "parentUuid": null,
            "message": {"content": [{"type": "text", "text": "question"}]}}),
        serde_json::json!({"type": "user", "uuid": "summary-uuid",
            "parentUuid": "question-uuid",
            "isCompactSummary": true, "isVisibleInTranscriptOnly": true,
            "message": {"content": "## Summary\nwhat happened so far"}}),
        serde_json::json!({"type": "system", "subtype": "compact_boundary",
            "uuid": "boundary-uuid", "parentUuid": "summary-uuid", "isMeta": false,
            "content": "Conversation compacted",
            "compactMetadata": {"trigger": "auto", "preTokens": 154_000,
                "postTokens": 32_000, "messagesSummarized": 87}}),
        serde_json::json!({"type": "assistant", "uuid": "answer-uuid",
            "parentUuid": "boundary-uuid",
            "message": {"content": [{"type": "text", "text": "answer"}]}}),
    ];
    let content: Vec<String> = lines.iter().map(|l| l.to_string()).collect();

    let items = replayed_items(content.join("\n").as_bytes());

    assert_eq!(
        items,
        vec![
            Item::UserMessage {
                text: Some("question".into())
            },
            Item::Compaction {
                id: "compaction-boundary-uuid".into(),
                detail: Compaction {
                    trigger: Some(CompactionTrigger::Automatic),
                    pre_tokens: Some(154_000),
                    post_tokens: Some(32_000),
                    messages_summarized: Some(87),
                    user_context: None,
                    summary: Some("## Summary\nwhat happened so far".into()),
                },
            },
            Item::AgentMessage {
                id: "replay-message-0".into(),
                text: Some("answer".into())
            },
        ]
    );
}

#[test]
fn a_boundary_without_a_summary_turn_still_marks_the_break() {
    let lines = [
        serde_json::json!({"type": "system", "subtype": "compact_boundary",
            "compactMetadata": {"trigger": "manual", "preTokens": 90_000}}),
        serde_json::json!({"type": "assistant",
            "message": {"content": [{"type": "text", "text": "after"}]}}),
    ];
    let content: Vec<String> = lines.iter().map(|l| l.to_string()).collect();

    let items = replayed_items(content.join("\n").as_bytes());

    assert_eq!(
        items,
        vec![
            Item::Compaction {
                id: "replay-compaction-1".into(),
                detail: Compaction {
                    trigger: Some(CompactionTrigger::Manual),
                    pre_tokens: Some(90_000),
                    post_tokens: None,
                    messages_summarized: None,
                    user_context: None,
                    summary: None,
                },
            },
            Item::AgentMessage {
                id: "replay-message-0".into(),
                text: Some("after".into())
            },
        ]
    );
}

#[test]
fn a_summary_whose_boundary_never_reached_disk_keeps_its_row() {
    let line = serde_json::json!({"type": "user", "uuid": "s1",
        "isCompactSummary": true, "message": {"content": "partial summary"}});

    let items = replayed_items(line.to_string().as_bytes());

    assert_eq!(
        items,
        vec![Item::Compaction {
            id: "compaction-s1".into(),
            detail: Compaction {
                summary: Some("partial summary".into()),
                ..Compaction::default()
            },
        }]
    );
}

#[test]
fn prompt_wrappers_are_stripped() {
    assert_eq!(
        title_line("<system-reminder>injected context</system-reminder>real question"),
        Some("real question".to_string())
    );
    assert_eq!(
        title_line(
            "<command-message>opsx:apply</command-message>\n<command-name>/opsx:apply</command-name>"
        ),
        Some("opsx:apply".to_string())
    );
    assert_eq!(
        title_line("one two three four five six seven eight"),
        Some("one two three four five six".to_string())
    );
}

#[test]
fn replay_keeps_dialogue_and_preserves_tool_details() {
    let lines = [
        serde_json::json!({"type": "queue-operation", "operation": "enqueue"}),
        serde_json::json!({"type": "user",
            "message": {"content": [{"type": "text", "text": "question"}]}}),
        serde_json::json!({"type": "assistant", "message": {"content": [
            {"type": "thinking", "thinking": "checking files"},
            {"type": "tool_use", "id": "t1", "name": "Bash",
             "input": {"command": "cargo check", "description": "Check the workspace"}},
            {"type": "tool_use", "id": "t2", "name": "Read",
             "input": {"file_path": "src/lib.rs"}}]}}),
        serde_json::json!({"type": "user",
        "message": {"content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": "ok"},
            {"type": "tool_result", "tool_use_id": "t2",
             "is_error": true,
             "content": [{"type": "text", "text": "fn main() {}"}]}
        ]}}),
        serde_json::json!({"type": "assistant", "isSidechain": true,
            "message": {"content": [{"type": "text", "text": "subagent"}]}}),
        serde_json::json!({"type": "assistant",
            "message": {"content": [{"type": "text", "text": "answer"}]}}),
    ];
    let content: Vec<String> = lines.iter().map(|l| l.to_string()).collect();

    let items = replayed_items(content.join("\n").as_bytes());

    assert_eq!(
        items,
        vec![
            Item::UserMessage {
                text: Some("question".into())
            },
            Item::Reasoning {
                id: "replay-thinking-0".into(),
                summary: Some("checking files".into()),
            },
            Item::CommandExecution {
                id: "t1".into(),
                command: "cargo check".into(),
                purpose: Some("Check the workspace".into()),
                aggregated_output: Some("ok".into()),
                status: Some("completed".into()),
                exit_code: None,
            },
            Item::Other {
                id: "t2".into(),
                kind: "Read".into(),
                title: "src/lib.rs".into(),
                output: Some("fn main() {}".into()),
                status: Some("failed".into()),
            },
            Item::AgentMessage {
                id: "replay-message-0".into(),
                text: Some("answer".into())
            },
        ]
    );
}

#[test]
fn replay_preserves_api_error_semantics() {
    let line = serde_json::json!({
        "type": "assistant",
        "isApiErrorMessage": true,
        "error": "rate_limit",
        "message": {
            "role": "assistant",
            "content": [{
                "type": "text",
                "text": "You've hit your session limit · resets 3:20pm (Asia/Shanghai)"
            }]
        }
    });

    let items = replayed_items(line.to_string().as_bytes());

    assert_eq!(
        items,
        vec![Item::Error {
            text: "You've hit your session limit · resets 3:20pm (Asia/Shanghai)".into()
        }]
    );
}

fn task_history_lines(records: &[Value]) -> String {
    records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn assistant_launch(uuid: &str, parent: Option<&str>, tool_use_id: &str) -> Value {
    serde_json::json!({
        "type": "assistant",
        "uuid": uuid,
        "parentUuid": parent,
        "timestamp": "2026-08-10T12:00:00Z",
        "message": {"role": "assistant", "content": [{
            "type": "tool_use",
            "id": tool_use_id,
            "name": "Task",
            "input": {
                "description": "Review the diff",
                "prompt": "Read the changed files",
                "subagent_type": "code-reviewer",
            },
        }]},
    })
}

fn tool_result_record(uuid: &str, parent: &str, tool_use_id: &str, is_error: bool) -> Value {
    serde_json::json!({
        "type": "user",
        "uuid": uuid,
        "parentUuid": parent,
        "timestamp": "2026-08-10T12:05:00Z",
        "message": {"role": "user", "content": [{
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "is_error": is_error,
            "content": "done",
        }]},
    })
}

#[test]
fn task_history_restores_completed_and_failed_children() {
    let lines = task_history_lines(&[
        serde_json::json!({"type": "user", "uuid": "u1", "parentUuid": null,
            "message": {"role": "user", "content": [{"type": "text", "text": "start"}]}}),
        assistant_launch("a1", Some("u1"), "toolu_ok"),
        tool_result_record("r1", "a1", "toolu_ok", false),
        assistant_launch("a2", Some("r1"), "toolu_bad"),
        tool_result_record("r2", "a2", "toolu_bad", true),
    ]);

    let tasks = parse_task_history(lines.as_bytes());

    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].id, "toolu_ok");
    assert_eq!(tasks[0].update.state, Some(BackgroundTaskState::Done));
    assert_eq!(
        tasks[0].update.display_name.as_deref(),
        Some("Review the diff")
    );
    assert_eq!(tasks[0].update.agent_type.as_deref(), Some("code-reviewer"));
    assert!(tasks[0].update.started_at.is_some());
    assert!(tasks[0].update.completed_at.is_some());
    assert_eq!(tasks[1].id, "toolu_bad");
    assert_eq!(tasks[1].update.state, Some(BackgroundTaskState::Failed));
}

#[test]
fn task_history_enriches_only_linked_sidechains() {
    let lines = task_history_lines(&[
        assistant_launch("a1", None, "toolu_known"),
        serde_json::json!({
            "type": "assistant",
            "uuid": "s1",
            "parentUuid": "a1",
            "isSidechain": true,
            "parent_tool_use_id": "toolu_known",
            "timestamp": "2026-08-10T12:02:00Z",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "read   src/lib.rs"}]},
        }),
        serde_json::json!({
            "type": "assistant",
            "uuid": "s2",
            "parentUuid": null,
            "isSidechain": true,
            "parent_tool_use_id": "toolu_abandoned",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "orphan"}]},
        }),
        tool_result_record("r1", "a1", "toolu_known", false),
    ]);

    let tasks = parse_task_history(lines.as_bytes());

    assert_eq!(tasks.len(), 1, "an unlinked sidechain never creates a row");
    assert_eq!(tasks[0].update.status.as_deref(), Some("read src/lib.rs"));
    assert_eq!(
        tasks[0].update.last_preview.as_deref(),
        Some("read src/lib.rs")
    );
}

#[test]
fn task_history_keeps_rows_that_lack_optional_metadata() {
    let lines = task_history_lines(&[serde_json::json!({
        "type": "assistant",
        "uuid": "a1",
        "parentUuid": null,
        "message": {"role": "assistant", "content": [{
            "type": "tool_use", "id": "toolu_bare", "name": "Task", "input": {},
        }]},
    })]);

    let tasks = parse_task_history(lines.as_bytes());

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, "toolu_bare");
    assert_eq!(tasks[0].update.state, Some(BackgroundTaskState::Starting));
    assert!(tasks[0].update.display_name.is_none());
    assert!(tasks[0].update.objective.is_none());
    assert!(tasks[0].update.started_at.is_none());
}

#[test]
fn task_history_applies_recognized_lifecycle_records() {
    let lines = task_history_lines(&[
        assistant_launch("a1", None, "toolu_1"),
        serde_json::json!({
            "type": "system",
            "subtype": "task_progress",
            "uuid": "l1",
            "parentUuid": "a1",
            "tool_use_id": "toolu_1",
            "task_type": "local_agent",
            "last_tool_name": "Grep",
            "timestamp": "2026-08-10T12:03:00Z",
        }),
    ]);

    let tasks = parse_task_history(lines.as_bytes());

    assert_eq!(tasks[0].update.state, Some(BackgroundTaskState::Working));
    assert_eq!(tasks[0].update.status.as_deref(), Some("Grep"));
}

#[test]
fn task_history_ignores_lifecycle_records_for_non_agent_work() {
    let lines = task_history_lines(&[
        assistant_launch("a1", None, "toolu_1"),
        serde_json::json!({
            "type": "system",
            "subtype": "task_notification",
            "uuid": "l1",
            "parentUuid": "a1",
            "tool_use_id": "toolu_1",
            "task_type": "local_bash",
            "status": "stopped",
        }),
    ]);

    let tasks = parse_task_history(lines.as_bytes());

    assert_eq!(
        tasks[0].update.state,
        Some(BackgroundTaskState::Starting),
        "a background shell's record must not move a child agent's row"
    );
}

#[test]
fn task_history_reads_a_killed_task_from_its_update_patch() {
    let lines = task_history_lines(&[
        assistant_launch("a1", None, "toolu_1"),
        serde_json::json!({
            "type": "system",
            "subtype": "task_updated",
            "uuid": "l1",
            "parentUuid": "a1",
            "tool_use_id": "toolu_1",
            "patch": {"status": "killed"},
            "timestamp": "2026-08-10T12:04:00Z",
        }),
    ]);

    let tasks = parse_task_history(lines.as_bytes());

    assert_eq!(tasks[0].update.state, Some(BackgroundTaskState::Stopped));
    assert!(tasks[0].update.completed_at.is_some());
}

#[test]
fn an_interrupted_history_stops_children_the_next_process_never_confirmed() {
    let lines = task_history_lines(&[
        assistant_launch("a1", None, "toolu_1"),
        serde_json::json!({"type": "system", "subtype": "init", "uuid": "i1", "parentUuid": "a1"}),
    ]);

    let tasks = parse_task_history(lines.as_bytes());

    assert_eq!(tasks[0].update.state, Some(BackgroundTaskState::Stopped));
}

#[test]
fn a_session_with_no_transcript_yet_restores_nothing_instead_of_failing() {
    // A conversation whose first turn has not written records yet has no file
    // on disk. Reporting that as a failure would show the panel as unavailable
    // for every brand-new session.
    let restored = load_task_history(Some("Z:/definitely/not/a/project"), "missing-session");

    assert_eq!(restored, Ok(Vec::new()));
}

#[test]
fn task_history_rebuilds_a_childs_own_conversation_from_linked_sidechains() {
    let lines = task_history_lines(&[
        assistant_launch("a1", None, "toolu_1"),
        serde_json::json!({
            "type": "user",
            "uuid": "s0",
            "parentUuid": "a1",
            "isSidechain": true,
            "parent_tool_use_id": "toolu_1",
            "message": {"role": "user", "content": [
                {"type": "text", "text": "review the parser"},
            ]},
        }),
        serde_json::json!({
            "type": "assistant",
            "uuid": "s1",
            "parentUuid": "s0",
            "isSidechain": true,
            "parent_tool_use_id": "toolu_1",
            "message": {"role": "assistant", "content": [
                {"type": "thinking", "id": "th-1", "thinking": "planning"},
                {"type": "tool_use", "id": "tu-1", "name": "Read", "input": {"file_path": "src/lib.rs"}},
            ]},
        }),
        serde_json::json!({
            "type": "user",
            "uuid": "s2",
            "parentUuid": "s1",
            "isSidechain": true,
            "parent_tool_use_id": "toolu_1",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tu-1", "content": "fn main() {}"},
            ]},
        }),
        serde_json::json!({
            "type": "assistant",
            "uuid": "s3",
            "parentUuid": "s2",
            "isSidechain": true,
            "parent_tool_use_id": "toolu_1",
            "message": {"role": "assistant", "content": [
                {"type": "text", "id": "tx-1", "text": "no issues found"},
            ]},
        }),
        tool_result_record("r1", "a1", "toolu_1", false),
    ]);

    let tasks = parse_task_history(lines.as_bytes());

    assert_eq!(tasks.len(), 1);

    // Assert on the accumulated conversation rather than the raw records: a
    // tool result carries its call's id, and folding it into that row is what
    // the accumulator does for both live and restored content.
    let mut transcript = BackgroundTaskTranscript::default();
    assert!(transcript.restore(tasks[0].items.clone()));

    let ids: Vec<_> = transcript.items().iter().map(Item::id).collect();
    assert_eq!(ids, [None, Some("th-1"), Some("tu-1"), Some("tx-1")]);
    assert!(
        matches!(
            &transcript.items()[0],
            Item::UserMessage { text: Some(text) } if text == "review the parser"
        ),
        "the child's first instruction is shown as user input"
    );
    assert!(
        matches!(transcript.items()[1], Item::Reasoning { .. }),
        "the child's work uses the same row kinds as the parent conversation"
    );
}

#[test]
fn an_unlinked_sidechain_contributes_no_child_conversation() {
    let lines = task_history_lines(&[
        assistant_launch("a1", None, "toolu_known"),
        serde_json::json!({
            "type": "assistant",
            "uuid": "s1",
            "parentUuid": null,
            "isSidechain": true,
            "parent_tool_use_id": "toolu_abandoned",
            "message": {"role": "assistant", "content": [
                {"type": "text", "id": "tx-9", "text": "orphan branch"},
            ]},
        }),
        tool_result_record("r1", "a1", "toolu_known", false),
    ]);

    let tasks = parse_task_history(lines.as_bytes());

    assert_eq!(tasks.len(), 1);
    assert!(
        tasks[0].items.is_empty(),
        "a branch no selected launch claims is not this child's conversation"
    );
}

/// Claude Code repeats current title metadata as the transcript grows. Only
/// the tail is read, so each title kind keeps its newest value while a user
/// name remains more important than a later model name.
#[test]
fn a_session_uses_recorded_title_precedence() {
    use std::fs;

    let root = env::temp_dir().join(format!("nmt-session-name-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();

    let filler = format!(
        "{}\n",
        serde_json::json!({"type": "user", "padding": "x".repeat(4096)})
    );
    let named = |title: &str| {
        format!(
            "{}\n",
            serde_json::json!({"type": "custom-title", "customTitle": title})
        )
    };
    let generated = |title: &str| {
        format!(
            "{}\n",
            serde_json::json!({"type": "ai-title", "aiTitle": title})
        )
    };

    let mut transcript = named("first name");
    // Enough to push the first name out of the window the tail scan reads,
    // and to make that window start partway through a record.
    for _ in 0..32 {
        transcript.push_str(&filler);
    }
    transcript.push_str(&named("second name"));
    transcript.push_str(&filler);

    let renamed = root.join("renamed.jsonl");
    fs::write(&renamed, transcript).unwrap();
    assert_eq!(recorded_title(&renamed).as_deref(), Some("second name"));

    let generated_only = root.join("generated.jsonl");
    fs::write(
        &generated_only,
        format!(
            "{}{}",
            generated("first model name"),
            generated("model name")
        ),
    )
    .unwrap();
    assert_eq!(
        resolved_session_title(
            &generated_only,
            Some("prompt fallback".into()),
            "12345678-rest"
        ),
        "model name"
    );

    let user_named = root.join("user-named.jsonl");
    fs::write(
        &user_named,
        format!("{}{}", named("user name"), generated("later model name")),
    )
    .unwrap();
    assert_eq!(
        resolved_session_title(&user_named, Some("prompt fallback".into()), "12345678-rest"),
        "user name"
    );

    let untouched = root.join("untouched.jsonl");
    fs::write(&untouched, &filler).unwrap();
    assert_eq!(
        resolved_session_title(&untouched, Some("prompt fallback".into()), "12345678-rest"),
        "prompt fallback"
    );
    assert_eq!(
        resolved_session_title(&untouched, None, "12345678-rest"),
        "12345678"
    );

    fs::remove_dir_all(root).unwrap();
}

/// inside it: `<session-id>/subagents/agent-<id>.jsonl` holds the conversation
/// and `agent-<id>.meta.json` names the tool call that launched it. The parent
/// file contains no sidechain records at all, so a scan of it finds nothing.
#[test]
fn a_child_conversation_is_read_from_its_own_file_and_linked_by_metadata() {
    use std::fs;

    let root = env::temp_dir().join(format!("nmt-subagent-history-{}", Uuid::new_v4()));
    let session_id = "sess-abc";
    let subagents = root.join(session_id).join("subagents");
    fs::create_dir_all(&subagents).unwrap();

    fs::write(
        root.join(format!("{session_id}.jsonl")),
        task_history_lines(&[
            assistant_launch("a1", None, "toolu_1"),
            tool_result_record("r1", "a1", "toolu_1", false),
        ]),
    )
    .unwrap();
    fs::write(
        subagents.join("agent-abc123.meta.json"),
        serde_json::json!({
            "agentType": "Explore",
            "description": "Explore popup options",
            "toolUseId": "toolu_1",
            "spawnDepth": 1,
        })
        .to_string(),
    )
    .unwrap();
    // Every record in a child's own file is a sidechain record.
    fs::write(
        subagents.join("agent-abc123.jsonl"),
        task_history_lines(&[
            serde_json::json!({
                "type": "user",
                "uuid": "c0",
                "parentUuid": null,
                "isSidechain": true,
                "message": {"role": "user", "content": [
                    {"type": "text", "text": "find the popup component"},
                ]},
            }),
            serde_json::json!({
                "type": "assistant",
                "uuid": "c1",
                "parentUuid": "c0",
                "isSidechain": true,
                "message": {"role": "assistant", "content": [
                    {"type": "text", "text": "found the popup component"},
                ]},
            }),
        ]),
    )
    .unwrap();

    let tasks = load_task_history_at(&root, session_id).expect("history is readable");

    assert_eq!(tasks.len(), 1);
    assert_eq!(
        tasks[0].items.len(),
        2,
        "the child's conversation comes from its own file"
    );
    assert!(matches!(
        &tasks[0].items[0],
        Item::UserMessage { text: Some(text) } if text == "find the popup component"
    ));
    assert_eq!(
        tasks[0].update.agent_type.as_deref(),
        Some("code-reviewer"),
        "the launch that started the child describes it better than the file"
    );
    assert_eq!(
        tasks[0].update.depth,
        Some(1),
        "spawn depth is only recorded beside the conversation"
    );

    fs::remove_dir_all(&root).ok();
}

/// The parent stream carries a child's launch instruction and its lifecycle
/// records but none of its replies, so the row's conversation is read from the
/// child's own file on demand rather than accumulated from the stream.
#[test]
fn one_childs_conversation_is_readable_without_rebuilding_the_session() {
    use std::fs;

    let root = env::temp_dir().join(format!("nmt-subagent-read-{}", Uuid::new_v4()));
    let session_id = "sess-abc";
    let subagents = root.join(session_id).join("subagents");
    fs::create_dir_all(&subagents).unwrap();

    fs::write(
        subagents.join("agent-abc123.meta.json"),
        serde_json::json!({"toolUseId": "toolu_1"}).to_string(),
    )
    .unwrap();
    fs::write(
        subagents.join("agent-abc123.jsonl"),
        task_history_lines(&[serde_json::json!({
            "type": "assistant",
            "uuid": "c1",
            "parentUuid": null,
            "isSidechain": true,
            "message": {"role": "assistant", "content": [
                {"type": "text", "text": "found the popup component"},
            ]},
        })]),
    )
    .unwrap();

    let items =
        load_child_transcript_at(&root, session_id, "toolu_1").expect("the child wrote a file");
    assert!(matches!(
        &items[0],
        Item::AgentMessage { text: Some(text), .. } if text == "found the popup component"
    ));

    // A session that kept no child files leaves the row as the stream left it.
    assert_eq!(
        load_child_transcript_at(&root, session_id, "toolu_other"),
        None
    );
    assert_eq!(
        load_child_transcript_at(&root, "sess-other", "toolu_1"),
        None
    );

    fs::remove_dir_all(&root).ok();
}

#[test]
fn a_child_conversation_with_no_matching_launch_is_left_alone() {
    use std::fs;

    let root = env::temp_dir().join(format!("nmt-subagent-history-{}", Uuid::new_v4()));
    let session_id = "sess-abc";
    let subagents = root.join(session_id).join("subagents");
    fs::create_dir_all(&subagents).unwrap();

    fs::write(
        root.join(format!("{session_id}.jsonl")),
        task_history_lines(&[assistant_launch("a1", None, "toolu_1")]),
    )
    .unwrap();
    fs::write(
        subagents.join("agent-other.meta.json"),
        serde_json::json!({"toolUseId": "toolu_from_another_branch"}).to_string(),
    )
    .unwrap();

    let tasks = load_task_history_at(&root, session_id).expect("history is readable");

    assert_eq!(tasks.len(), 1);
    assert!(
        tasks[0].items.is_empty(),
        "a conversation whose launch is not in this history is not this child's"
    );

    fs::remove_dir_all(&root).ok();
}

/// The CLI injects background-agent status back into the conversation as a
/// synthesized `user` turn. It is addressed to the model, not written by the
/// person, so replaying it as a prompt shows content they never typed.
#[test]
fn task_notifications_are_not_replayed_as_user_prompts() {
    let notification = "<task-notification>\n<task-id>af51619832c5b38f5</task-id>\n\
                        <tool-use-id>toolu_01WjVSCJychqyGiWnTbV9jRg</tool-use-id>\n\
                        <status>completed</status>\n</task-notification>";
    let lines = task_history_lines(&[
        serde_json::json!({
            "type": "user",
            "uuid": "u1",
            "parentUuid": null,
            "isSidechain": false,
            "origin": {"kind": "human"},
            "message": {"role": "user", "content": "review the diff"},
        }),
        serde_json::json!({
            "type": "user",
            "uuid": "u2",
            "parentUuid": "u1",
            "isSidechain": false,
            "origin": {"kind": "task-notification"},
            "promptSource": "system",
            "message": {"role": "user", "content": notification},
        }),
    ]);

    let items = replayed_items(lines.as_bytes());

    assert_eq!(
        items,
        vec![Item::UserMessage {
            text: Some("review the diff".into())
        }]
    );
}

#[test]
fn an_interruption_marker_is_not_replayed_as_a_user_prompt() {
    let lines = task_history_lines(&[
        serde_json::json!({
            "type": "user",
            "uuid": "u1",
            "parentUuid": null,
            "isSidechain": false,
            "message": {"role": "user", "content": "run the tests"},
        }),
        serde_json::json!({
            "type": "user",
            "uuid": "u2",
            "parentUuid": "u1",
            "isSidechain": false,
            "interruptedMessageId": "msg_011Cd5hihs8xXo8zvdmiVkGL",
            "message": {"role": "user", "content": "[Request interrupted by user]"},
        }),
    ]);

    let items = replayed_items(lines.as_bytes());

    assert_eq!(
        items,
        vec![Item::UserMessage {
            text: Some("run the tests".into())
        }]
    );
}

#[test]
fn a_task_notification_without_an_origin_is_still_not_a_prompt() {
    // Older CLI versions recorded no origin, so the block itself is the marker.
    let lines = task_history_lines(&[serde_json::json!({
        "type": "user",
        "uuid": "u1",
        "parentUuid": null,
        "isSidechain": false,
        "message": {"role": "user", "content": "<task-notification>\n<status>completed</status>\n</task-notification>"},
    })]);

    assert!(replayed_items(lines.as_bytes()).is_empty());
}

#[test]
fn an_ordinary_prompt_mentioning_a_notification_is_still_a_prompt() {
    // Only the record's own origin, or a block it starts with, marks plumbing;
    // a person discussing one is still speaking.
    let lines = task_history_lines(&[serde_json::json!({
        "type": "user",
        "uuid": "u1",
        "parentUuid": null,
        "isSidechain": false,
        "origin": {"kind": "human"},
        "message": {"role": "user", "content": "why did the <task-notification> block appear?"},
    })]);

    assert_eq!(replayed_items(lines.as_bytes()).len(), 1);
}

#[test]
fn replay_divides_a_session_into_the_turns_it_recorded() {
    let lines = [
        serde_json::json!({"type": "user", "uuid": "u1", "parentUuid": null,
            "timestamp": "2026-08-07T01:00:00Z",
            "message": {"role": "user", "content": "first prompt"}}),
        serde_json::json!({"type": "assistant", "uuid": "a1", "parentUuid": "u1",
            "timestamp": "2026-08-07T01:00:42Z",
            "message": {"content": [{"type": "text", "text": "first answer"}],
                "usage": {"output_tokens": 120}}}),
        // Written beside the chain rather than in it: nothing links to it.
        serde_json::json!({"type": "system", "subtype": "turn_duration", "uuid": "d1",
            "parentUuid": "a1", "isMeta": false, "durationMs": 42_000, "messageCount": 2}),
        serde_json::json!({"type": "user", "uuid": "u2", "parentUuid": "a1",
            "timestamp": "2026-08-07T01:01:00Z",
            "message": {"role": "user", "content": "second prompt"}}),
        serde_json::json!({"type": "assistant", "uuid": "a2", "parentUuid": "u2",
            "message": {"content": [{"type": "text", "text": "second answer"}],
                "usage": {"output_tokens": 30}}}),
        serde_json::json!({"type": "user", "uuid": "i1", "parentUuid": "a2",
            "interruptedMessageId": "msg_1",
            "message": {"role": "user", "content": "[Request interrupted by user]"}}),
    ];
    let content: Vec<String> = lines.iter().map(|l| l.to_string()).collect();

    let turns = parse_replay(content.join("\n").as_bytes());

    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].seconds, Some(42));
    assert_eq!(turns[0].output_tokens, Some(120));
    assert!(!turns[0].interrupted);
    assert_eq!(
        turns[0].items[0].at,
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-08-07T01:00:00Z")
                .unwrap()
                .timestamp()
        )
    );
    assert_eq!(
        turns[0]
            .items
            .iter()
            .map(|entry| entry.item.clone())
            .collect::<Vec<_>>(),
        vec![
            Item::UserMessage {
                text: Some("first prompt".into())
            },
            Item::AgentMessage {
                id: "replay-message-0".into(),
                text: Some("first answer".into())
            },
        ]
    );

    // No duration record closed the second turn, and the user stopped it.
    assert_eq!(turns[1].seconds, None);
    assert_eq!(turns[1].output_tokens, Some(30));
    assert!(turns[1].interrupted);
    assert_eq!(turns[1].items.len(), 2);
}
