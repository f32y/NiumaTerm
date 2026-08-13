use std::path::{Path, PathBuf};
use std::{env, fs};

use serde_json::{Value, json};
use uuid::Uuid;

use crate::claude_code::tasks::ClaudeTasks;
use crate::claude_code::workflows::disk::{
    read_agent_transcript, read_run_snapshots_at, resolve_run_directory_at,
};
use crate::claude_code::workflows::*;
use crate::workflow::*;

const SESSION: &str = "62465c13-1807-4af0-a444-0317c1d429fa";
const TASK: &str = "wacob045a";
const RUN_ID: &str = "wf_36e84f35-a64";
const AGENT_ONE: &str = "a6a0cda9e93639379";
const AGENT_TWO: &str = "a5a25521c354f9bd7";

/// Captured from a real two-agent run. The transcript's one attachment record
/// held a skill listing that says nothing about the shape under test, so its
/// content is blanked while the record itself stays: dropping it would break
/// the `parentUuid` chain the replay parser walks.
const JOURNAL: &str = include_str!("../../../tests/fixtures/claude/workflow/journal.jsonl");
const TRANSCRIPT: &str =
    include_str!("../../../tests/fixtures/claude/workflow/agent-transcript.jsonl");
const RUN_SNAPSHOT: &str =
    include_str!("../../../tests/fixtures/claude/workflow/run-snapshot.json");

fn reducer() -> ClaudeWorkflows {
    let mut workflows = ClaudeWorkflows::default();
    workflows.set_session(SESSION);
    workflows
}

fn started(extra: Value) -> Value {
    let mut record = json!({
        "type": "system",
        "subtype": "task_started",
        "session_id": SESSION,
        "task_id": TASK,
        "task_type": "local_workflow",
        "workflow_name": "two-ok",
    });
    merge(&mut record, extra);
    record
}

fn progress(progress_entries: Value) -> Value {
    json!({
        "type": "system",
        "subtype": "task_progress",
        "session_id": SESSION,
        "task_id": TASK,
        "workflow_progress": progress_entries,
        "usage": {"total_tokens": 31_154, "tool_uses": 0},
    })
}

fn agent_entry(index: u64, agent_id: &str, state: &str, extra: Value) -> Value {
    let mut entry = json!({
        "type": "workflow_agent",
        "index": index,
        "label": format!("ok-{index}"),
        "phaseIndex": 1,
        "phaseTitle": "Ok",
        "agentId": agent_id,
        "model": "claude-fable-5",
        "state": state,
        "queuedAt": 1_786_606_968_695u64,
    });
    merge(&mut entry, extra);
    entry
}

fn merge(target: &mut Value, extra: Value) {
    let (Some(target), Some(extra)) = (target.as_object_mut(), extra.as_object()) else {
        return;
    };
    for (key, value) in extra {
        target.insert(key.clone(), value.clone());
    }
}

fn only_run(workflows: &ClaudeWorkflows) -> WorkflowRun {
    let snapshot = workflows.snapshot().expect("session is known");
    assert_eq!(snapshot.runs.len(), 1, "expected exactly one run");
    snapshot.runs.into_iter().next().expect("one run")
}

#[test]
fn a_run_appears_before_any_agent_reports() {
    let mut workflows = reducer();
    assert!(workflows.observe(&started(json!({}))));

    let run = only_run(&workflows);
    assert_eq!(run.task_id, TASK);
    assert_eq!(run.name.as_deref(), Some("two-ok"));
    assert_eq!(run.state, WorkflowRunState::Starting);
    assert!(run.agents.is_empty());
    assert!(run.phases.is_empty());
    assert!(workflows.snapshot().expect("session").has_active_run());
}

#[test]
fn only_the_workflow_task_type_opens_a_run() {
    for task_type in [
        "local_agent",
        "local_bash",
        "monitor_mcp",
        "in_process_teammate",
    ] {
        let mut workflows = reducer();
        assert!(!workflows.observe(&started(json!({"task_type": task_type}))));
        assert!(workflows.snapshot().expect("session").runs.is_empty());
    }
}

#[test]
fn phases_and_agents_arrive_from_the_progress_array() {
    let mut workflows = reducer();
    workflows.observe(&started(json!({})));
    assert!(workflows.observe(&progress(json!([
        {"type": "workflow_phase", "index": 1, "title": "Ok"},
        agent_entry(2, AGENT_TWO, "start", json!({"startedAt": 1_786_606_971_533u64})),
        agent_entry(1, AGENT_ONE, "done", json!({"tokens": 15_577, "toolCalls": 0, "resultPreview": "ok"})),
    ]))));

    let run = only_run(&workflows);
    assert_eq!(run.state, WorkflowRunState::Running);
    assert_eq!(
        run.phases,
        vec![WorkflowPhase {
            index: 1,
            title: "Ok".into()
        }]
    );
    assert_eq!(run.total_tokens, Some(31_154));
    assert_eq!(run.total_tool_calls, Some(0));

    // Provider order wins over arrival order.
    let labels: Vec<_> = run.agents.iter().map(|agent| agent.index).collect();
    assert_eq!(labels, vec![1, 2]);

    assert_eq!(run.agents[0].state, WorkflowAgentState::Done);
    assert_eq!(run.agents[0].tokens, Some(15_577));
    assert_eq!(run.agents[0].result_preview.as_deref(), Some("ok"));
    assert_eq!(run.agents[0].phase_title.as_deref(), Some("Ok"));
    assert_eq!(run.agents[1].state, WorkflowAgentState::Running);
    // Unreported details stay absent instead of being defaulted.
    assert_eq!(run.agents[1].tokens, None);
    assert_eq!(run.agents[1].agent_type, None);
}

#[test]
fn a_queued_agent_is_distinguished_from_a_running_one() {
    let mut workflows = reducer();
    workflows.observe(&started(json!({})));
    workflows.observe(&progress(json!([
        agent_entry(1, AGENT_ONE, "start", json!({})),
        agent_entry(
            2,
            AGENT_TWO,
            "start",
            json!({"startedAt": 1_786_606_971_533u64})
        ),
    ])));

    let run = only_run(&workflows);
    assert_eq!(run.agents[0].state, WorkflowAgentState::Queued);
    assert_eq!(run.agents[1].state, WorkflowAgentState::Running);
}

#[test]
fn a_failed_agent_keeps_its_error_and_a_reused_one_is_marked() {
    let mut workflows = reducer();
    workflows.observe(&started(json!({})));
    workflows.observe(&progress(json!([
        agent_entry(1, AGENT_ONE, "error", json!({"error": "spawn refused"})),
        agent_entry(2, AGENT_TWO, "done", json!({"cached": true})),
    ])));

    let run = only_run(&workflows);
    assert_eq!(run.agents[0].state, WorkflowAgentState::Failed);
    assert_eq!(run.agents[0].error.as_deref(), Some("spawn refused"));
    assert!(!run.agents[0].reused);
    assert_eq!(run.agents[1].state, WorkflowAgentState::Done);
    assert!(run.agents[1].reused);
}

#[test]
fn later_records_match_their_run_without_a_task_type() {
    // Only `task_started` carries `task_type`; every later record identifies
    // its run by `task_id` alone.
    let mut workflows = reducer();
    workflows.observe(&started(json!({})));

    let mut update = progress(json!([]));
    assert!(update["task_type"].is_null());
    assert!(workflows.observe(&update));
    assert_eq!(only_run(&workflows).state, WorkflowRunState::Running);

    // The same record for a run that was never opened is not ours.
    update["task_id"] = json!("unknown-task");
    assert!(!workflows.observe(&update));
    assert_eq!(workflows.snapshot().expect("session").runs.len(), 1);
}

#[test]
fn a_terminal_record_settles_the_run_and_keeps_its_final_text() {
    let mut workflows = reducer();
    workflows.observe(&started(json!({})));
    assert!(workflows.observe(&json!({
        "type": "system",
        "subtype": "task_notification",
        "session_id": SESSION,
        "task_id": TASK,
        "status": "completed",
        "summary": "Two trivial agents each return ok",
    })));

    let run = only_run(&workflows);
    assert_eq!(run.state, WorkflowRunState::Done);
    assert_eq!(
        run.result.as_deref(),
        Some("Two trivial agents each return ok")
    );
    assert!(!workflows.snapshot().expect("session").has_active_run());
}

#[test]
fn switching_sessions_drops_the_previous_sessions_runs() {
    let mut workflows = reducer();
    workflows.observe(&started(json!({})));
    assert_eq!(workflows.snapshot().expect("session").runs.len(), 1);

    workflows.set_session("another-session");
    let snapshot = workflows.snapshot().expect("session");
    assert_eq!(snapshot.session_id, "another-session");
    assert!(snapshot.runs.is_empty());
}

#[test]
fn a_workflow_run_still_produces_no_background_task_row() {
    // The child-agent view excludes workflows; reading the same records here
    // must not change that.
    let mut tasks = ClaudeTasks::default();
    tasks.observe(&json!({"type": "system", "subtype": "init", "session_id": SESSION}));
    assert!(!tasks.observe(&started(json!({}))));
    assert!(!tasks.observe(&progress(json!([agent_entry(
        1,
        AGENT_ONE,
        "done",
        json!({})
    ),]))));

    assert!(tasks.snapshot().expect("session is known").tasks.is_empty());
}

// ---------------------------------------------------------------- disk reads

/// Lay out one run the way the provider does, from the captured fixtures.
fn run_tree() -> PathBuf {
    let root = env::temp_dir().join(format!("nmt-workflow-{}", Uuid::new_v4()));
    let run_dir = root
        .join(SESSION)
        .join("subagents")
        .join("workflows")
        .join(RUN_ID);
    fs::create_dir_all(&run_dir).expect("create run dir");
    fs::write(run_dir.join("journal.jsonl"), JOURNAL).expect("write journal");
    fs::write(run_dir.join(format!("agent-{AGENT_TWO}.jsonl")), TRANSCRIPT).expect("write agent");

    let snapshots = root.join(SESSION).join("workflows");
    fs::create_dir_all(&snapshots).expect("create snapshot dir");
    fs::write(snapshots.join(format!("{RUN_ID}.json")), RUN_SNAPSHOT).expect("write snapshot");
    root
}

fn run_dir_of(root: &Path) -> PathBuf {
    root.join(SESSION)
        .join("subagents")
        .join("workflows")
        .join(RUN_ID)
}

#[test]
fn the_completion_snapshot_resolves_the_run_directory_by_task_id() {
    let root = run_tree();
    let resolved = resolve_run_directory_at(&root, SESSION, TASK, &[]);

    assert_eq!(resolved.as_deref(), Some(run_dir_of(&root).as_path()));
    fs::remove_dir_all(&root).ok();
}

#[test]
fn a_live_run_resolves_its_directory_by_agent_id() {
    let root = run_tree();
    // A run still in progress has written no completion snapshot yet.
    fs::remove_dir_all(root.join(SESSION).join("workflows")).expect("drop snapshots");

    assert_eq!(
        resolve_run_directory_at(&root, SESSION, TASK, &[AGENT_TWO.to_owned()]).as_deref(),
        Some(run_dir_of(&root).as_path())
    );
    // Without a known agent there is nothing to match on yet.
    assert_eq!(resolve_run_directory_at(&root, SESSION, TASK, &[]), None);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn the_journal_reports_each_agents_completion() {
    let root = run_tree();
    let entries = read_journal(&run_dir_of(&root)).expect("journal reads");

    assert_eq!(entries.len(), 2);
    for entry in &entries {
        assert_eq!(entry.result.as_deref(), Some("ok"), "{entry:?}");
    }
    assert!(entries.iter().any(|entry| entry.agent_id == AGENT_ONE));
    assert!(entries.iter().any(|entry| entry.agent_id == AGENT_TWO));
    fs::remove_dir_all(&root).ok();
}

#[test]
fn a_truncated_journal_line_is_dropped_rather_than_failing_the_read() {
    let root = run_tree();
    let path = run_dir_of(&root).join("journal.jsonl");
    let mut text = fs::read_to_string(&path).expect("read journal");
    text.push_str("{\"type\":\"result\",\"agentId\":\"a5a2");
    fs::write(&path, text).expect("append partial line");

    let entries = read_journal(&run_dir_of(&root)).expect("partial journal still reads");
    assert_eq!(entries.len(), 2);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn a_missing_journal_is_reported_without_panicking() {
    let root = run_tree();
    fs::remove_file(run_dir_of(&root).join("journal.jsonl")).expect("drop journal");

    assert!(read_journal(&run_dir_of(&root)).is_err());
    fs::remove_dir_all(&root).ok();
}

#[test]
fn an_agent_transcript_parses_into_conversation_items() {
    let root = run_tree();
    let items = read_agent_transcript(&run_dir_of(&root), AGENT_TWO).expect("transcript reads");

    assert!(
        !items.is_empty(),
        "the captured workflow transcript should parse into items"
    );
    assert!(read_agent_transcript(&run_dir_of(&root), AGENT_ONE).is_err());
    fs::remove_dir_all(&root).ok();
}

#[test]
fn a_grown_transcript_is_detectable_without_reparsing() {
    let root = run_tree();
    let before = agent_transcript_len(&run_dir_of(&root), AGENT_TWO).expect("size");
    fs::write(
        run_dir_of(&root).join(format!("agent-{AGENT_TWO}.jsonl")),
        format!("{TRANSCRIPT}{TRANSCRIPT}"),
    )
    .expect("grow transcript");

    let after = agent_transcript_len(&run_dir_of(&root), AGENT_TWO).expect("size");
    assert!(after > before);
    assert_eq!(agent_transcript_len(&run_dir_of(&root), AGENT_ONE), None);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn a_resumed_session_restores_its_completed_runs() {
    let root = run_tree();
    let restored = read_run_snapshots_at(&root, SESSION).expect("snapshots read");

    assert_eq!(restored.len(), 1);
    let run = &restored[0].run;
    assert_eq!(run.task_id, TASK);
    assert_eq!(run.run_id.as_deref(), Some(RUN_ID));
    assert_eq!(run.name.as_deref(), Some("two-ok"));
    assert_eq!(run.state, WorkflowRunState::Done);
    assert_eq!(run.agents.len(), 2);
    assert_eq!(run.phases.len(), 1);
    assert_eq!(run.total_tokens, Some(31_154));
    // The provider records the outcome as one entry per agent.
    assert_eq!(run.result.as_deref(), Some("ok\nok"));
    fs::remove_dir_all(&root).ok();
}

#[test]
fn a_session_that_ran_no_workflow_restores_nothing() {
    let root = env::temp_dir().join(format!("nmt-workflow-{}", Uuid::new_v4()));
    fs::create_dir_all(root.join(SESSION)).expect("create session dir");

    assert_eq!(read_run_snapshots_at(&root, SESSION), Ok(Vec::new()));
    fs::remove_dir_all(&root).ok();
}

#[test]
fn a_journal_refresh_advances_agents_the_stream_has_not_settled() {
    let mut workflows = reducer();
    workflows.observe(&started(json!({})));
    workflows.observe(&progress(json!([
        agent_entry(
            1,
            AGENT_ONE,
            "start",
            json!({"startedAt": 1_786_606_970_440u64})
        ),
        agent_entry(2, AGENT_TWO, "start", json!({})),
    ])));

    assert!(workflows.apply_refresh(
        TASK,
        WorkflowRefresh {
            run_id: Some(RUN_ID.to_owned()),
            journal: vec![
                WorkflowJournalEntry {
                    agent_id: AGENT_ONE.to_owned(),
                    result: Some("ok".to_owned()),
                },
                WorkflowJournalEntry {
                    agent_id: AGENT_TWO.to_owned(),
                    result: None,
                },
            ],
            failed: false,
        },
    ));

    let run = only_run(&workflows);
    assert_eq!(run.run_id.as_deref(), Some(RUN_ID));
    assert_eq!(run.agents[0].state, WorkflowAgentState::Done);
    assert_eq!(run.agents[0].result_preview.as_deref(), Some("ok"));
    // A journal `started` line moves a queued agent forward, never backward.
    assert_eq!(run.agents[1].state, WorkflowAgentState::Running);
}

#[test]
fn a_failed_refresh_is_reported_without_touching_known_state() {
    let mut workflows = reducer();
    workflows.observe(&started(json!({})));
    workflows.observe(&progress(json!([agent_entry(
        1,
        AGENT_ONE,
        "done",
        json!({"tokens": 15_577})
    ),])));

    assert!(workflows.apply_refresh(
        TASK,
        WorkflowRefresh {
            failed: true,
            ..WorkflowRefresh::default()
        },
    ));

    let run = only_run(&workflows);
    assert!(run.refresh_failed);
    assert_eq!(run.state, WorkflowRunState::Running);
    assert_eq!(run.agents[0].state, WorkflowAgentState::Done);
    assert_eq!(run.agents[0].tokens, Some(15_577));
}

#[test]
fn restored_runs_never_replace_a_run_the_stream_already_reported() {
    let mut workflows = reducer();
    workflows.observe(&started(json!({})));
    workflows.observe(&progress(json!([])));

    let root = run_tree();
    let restored = read_run_snapshots_at(&root, SESSION).expect("snapshots read");
    assert!(!workflows.merge_restored(restored));

    let run = only_run(&workflows);
    assert_eq!(run.state, WorkflowRunState::Running);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn a_run_whose_snapshot_never_landed_still_restores() {
    // A run the process outlived writes no completion snapshot, so the run
    // directory is the only record left of it.
    let root = run_tree();
    fs::remove_dir_all(root.join(SESSION).join("workflows")).expect("drop snapshots");
    let scripts = root.join(SESSION).join("workflows").join("scripts");
    fs::create_dir_all(&scripts).expect("create scripts dir");
    fs::write(
        scripts.join(format!("deep-research-{RUN_ID}.js")),
        "// script",
    )
    .expect("write script copy");

    // A second agent that never reached the journal: its transcript is the
    // only evidence it ran at all.
    let orphan = "a0102e30fb830b0c4";
    fs::write(
        run_dir_of(&root).join(format!("agent-{orphan}.jsonl")),
        TRANSCRIPT,
    )
    .expect("write orphan transcript");

    let restored = read_run_snapshots_at(&root, SESSION).expect("snapshots read");
    assert_eq!(restored.len(), 1);

    let run = &restored[0].run;
    // With no snapshot there is no stream task id, so the directory names it.
    assert_eq!(run.task_id, RUN_ID);
    assert_eq!(run.run_id.as_deref(), Some(RUN_ID));
    assert_eq!(run.name.as_deref(), Some("deep-research"));
    assert_eq!(run.state, WorkflowRunState::Stopped);
    // The snapshot is what carries these, so they stay absent.
    assert!(run.phases.is_empty());
    assert_eq!(run.total_tokens, None);

    assert_eq!(run.agents.len(), 2);
    let orphan_row = run
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_deref() == Some(orphan))
        .expect("orphan agent is listed");
    // The journal never mentioned it, so it reports as cut off rather than
    // done or failed.
    assert_eq!(orphan_row.state, WorkflowAgentState::Stopped);

    let journaled = run
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_deref() == Some(AGENT_TWO))
        .expect("journaled agent is listed");
    assert_eq!(journaled.state, WorkflowAgentState::Done);

    // Both are labelled from the opening line of the prompt they were given.
    for agent in &run.agents {
        let label = agent.label.as_deref().expect("agent carries a label");
        assert!(!label.is_empty());
        assert!(!label.starts_with('#'), "{label}");
    }

    fs::remove_dir_all(&root).ok();
}

#[test]
fn a_completed_run_is_not_restored_twice() {
    // The completion snapshot and the run directory describe the same run.
    let root = run_tree();
    let restored = read_run_snapshots_at(&root, SESSION).expect("snapshots read");

    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].run.task_id, TASK);
    assert_eq!(restored[0].run.state, WorkflowRunState::Done);
    fs::remove_dir_all(&root).ok();
}
