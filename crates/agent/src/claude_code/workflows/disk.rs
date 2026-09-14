//! Reading a workflow run's persisted record.
//!
//! The stream carries run and agent state but never conversation, so the only
//! source for what an agent actually said is the transcript the provider
//! writes. Two on-disk records matter, and they serve different phases:
//!
//! - `<session>/subagents/workflows/<run-id>/journal.jsonl` is appended while
//!   the run is live and is small enough to poll every second.
//! - `<session>/workflows/<run-id>.json` is a completion snapshot, written
//!   after the run ends. It is the only direct `taskId` -> `runId` mapping and
//!   the restoration source, and it is useless for live refreshing.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use parking_lot::Mutex;
use serde_json::Value;

use crate::chat::Item;
use crate::claude_code::sessions::{parse_child_replay, project_dir};
use crate::claude_code::workflows::parse_progress;
use crate::json::text_field;
use crate::workflow::{
    WorkflowAgent, WorkflowAgentProgress, WorkflowAgentState, WorkflowRefreshRequest,
    WorkflowRefreshResult, WorkflowRun, WorkflowRunState, WorkflowSource, WorkflowTranscriptRead,
};

/// One agent's line in a run journal. `result` is present once the agent has
/// finished, which is what makes the journal worth polling: it reports a
/// completion the stream may not mention for another few seconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WorkflowJournalEntry {
    pub(super) agent_id: String,
    pub(super) result: Option<String>,
}

/// Directory holding one run's per-agent transcripts and journal.
fn resolve_run_directory(
    cwd: Option<&str>,
    session_id: &str,
    task_id: &str,
    agent_ids: &[String],
) -> Option<PathBuf> {
    let project = project_dir(cwd)?;

    resolve_run_directory_at(&project, session_id, task_id, agent_ids)
}

pub(super) fn resolve_run_directory_at(
    project: &Path,
    session_id: &str,
    task_id: &str,
    agent_ids: &[String],
) -> Option<PathBuf> {
    let session = project.join(session_id);

    // A finished run names its own directory, so prefer that over searching.
    if let Some(run_id) = run_id_for_task(&session, task_id) {
        let dir = session.join("subagents").join("workflows").join(&run_id);

        if dir.is_dir() {
            return Some(dir);
        }
    }

    // A live run has no snapshot yet. Agent ids are unique, so the directory
    // holding one of this run's transcripts is this run's directory.
    let workflows = session.join("subagents").join("workflows");

    for entry in fs::read_dir(&workflows).ok()?.flatten() {
        let dir = entry.path();

        if !dir.is_dir() {
            continue;
        }

        if agent_ids
            .iter()
            .any(|agent_id| dir.join(format!("agent-{agent_id}.jsonl")).is_file())
        {
            return Some(dir);
        }
    }

    None
}

/// `runId` of the completion snapshot recorded for this stream task.
fn run_id_for_task(session: &Path, task_id: &str) -> Option<String> {
    for entry in fs::read_dir(session.join("workflows")).ok()?.flatten() {
        let path = entry.path();

        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }

        let Some(snapshot) = read_json(&path) else {
            continue;
        };

        if snapshot["taskId"].as_str() == Some(task_id) {
            return text_field(&snapshot, &["runId"]);
        }
    }

    None
}

/// Per-agent progress recorded in a live run's journal.
///
/// A journal being appended to as this reads can end mid-line; the records
/// read so far are still valid, so a trailing partial line is dropped rather
/// than failing the refresh.
pub(super) fn read_journal(dir: &Path) -> Result<Vec<WorkflowJournalEntry>, String> {
    let path = dir.join("journal.jsonl");
    let file = fs::File::open(&path).map_err(|error| format!("{}: {error}", path.display()))?;

    let mut entries: Vec<WorkflowJournalEntry> = Vec::new();

    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };

        let Some(agent_id) = text_field(&record, &["agentId"]) else {
            continue;
        };

        let result = match record["type"].as_str() {
            Some("started") => None,
            Some("result") => text_field(&record, &["result"]).or_else(|| Some(String::new())),
            _ => continue,
        };

        match entries.iter_mut().find(|entry| entry.agent_id == agent_id) {
            // A later line for the same agent supersedes the earlier one, so a
            // result never loses to the start that preceded it.
            Some(entry) => {
                if result.is_some() {
                    entry.result = result;
                }
            }
            None => entries.push(WorkflowJournalEntry { agent_id, result }),
        }
    }

    Ok(entries)
}

/// One agent's own conversation, in the items the parent transcript renders.
pub(super) fn read_agent_transcript(dir: &Path, agent_id: &str) -> Result<Vec<Item>, String> {
    let path = dir.join(format!("agent-{agent_id}.jsonl"));
    let file = fs::File::open(&path).map_err(|error| format!("{}: {error}", path.display()))?;

    Ok(parse_child_replay(BufReader::new(file)))
}

/// Size of an agent transcript, so a caller can skip re-parsing a file that
/// has not grown since it last read it.
pub(super) fn agent_transcript_len(dir: &Path, agent_id: &str) -> Option<u64> {
    fs::metadata(dir.join(format!("agent-{agent_id}.jsonl")))
        .ok()
        .map(|metadata| metadata.len())
}

/// File observations remain private to the reader. A consumer acknowledges a
/// revision only after accepting the result, so discarded reads can be retried.
#[derive(Default)]
pub(crate) struct ClaudeWorkflowSource {
    cache: Mutex<TranscriptCache>,
}

#[derive(Default)]
struct TranscriptCache {
    next_revision: u64,
    transcripts: HashMap<PathBuf, (u64, u64)>,
}

impl WorkflowSource for ClaudeWorkflowSource {
    fn restore(&self, cwd: Option<&str>, session_id: &str) -> Result<Vec<WorkflowRun>, String> {
        read_run_snapshots(cwd, session_id)
    }

    fn refresh(
        &self,
        cwd: Option<&str>,
        session_id: &str,
        request: &WorkflowRefreshRequest,
    ) -> WorkflowRefreshResult {
        let dir = resolve_run_directory(cwd, session_id, &request.task_id, &request.agent_ids);

        self.refresh_directory(dir.as_deref(), request)
    }
}

impl ClaudeWorkflowSource {
    pub(super) fn refresh_directory(
        &self,
        dir: Option<&Path>,
        request: &WorkflowRefreshRequest,
    ) -> WorkflowRefreshResult {
        let mut result = WorkflowRefreshResult {
            task_id: request.task_id.clone(),
            ..WorkflowRefreshResult::default()
        };

        let Some(dir) = dir else {
            return result;
        };

        result.refresh.run_id = dir
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned);

        match read_journal(dir) {
            Ok(journal) => {
                result.refresh.agents = journal
                    .into_iter()
                    .map(|entry| WorkflowAgentProgress {
                        agent_id: entry.agent_id,
                        state: if entry.result.is_some() {
                            WorkflowAgentState::Done
                        } else {
                            WorkflowAgentState::Running
                        },
                        result: entry.result,
                    })
                    .collect()
            }
            Err(_) => result.refresh.failed = true,
        }

        if let Some(agent_id) = request.open_agent.as_deref() {
            result.transcript = self.read_transcript(dir, agent_id, request.transcript_revision);
        }

        result
    }

    fn read_transcript(
        &self,
        dir: &Path,
        agent_id: &str,
        accepted_revision: Option<u64>,
    ) -> Option<WorkflowTranscriptRead> {
        let path = dir.join(format!("agent-{agent_id}.jsonl"));
        let len = agent_transcript_len(dir, agent_id)?;
        let previous = self.cache.lock().transcripts.get(&path).copied();

        if let Some((previous_len, revision)) = previous
            && previous_len == len
            && accepted_revision == Some(revision)
        {
            return None;
        }

        let items = read_agent_transcript(dir, agent_id).ok()?;

        let mut cache = self.cache.lock();

        // Another reader may have published while this transcript was read.
        // Leave its revision intact and retry instead of publishing older data.
        if cache.transcripts.get(&path).copied() != previous {
            return None;
        }

        let revision = match previous {
            Some((previous_len, revision)) if previous_len == len => revision,
            _ => {
                cache.next_revision += 1;

                let revision = cache.next_revision;

                cache.transcripts.insert(path, (len, revision));

                revision
            }
        };

        Some(WorkflowTranscriptRead {
            agent_id: agent_id.to_owned(),
            items,
            revision,
        })
    }
}

/// Every run a resumed session completed, newest last.
fn read_run_snapshots(cwd: Option<&str>, session_id: &str) -> Result<Vec<WorkflowRun>, String> {
    let project =
        project_dir(cwd).ok_or_else(|| format!("session {session_id} has no project directory"))?;

    read_run_snapshots_at(&project, session_id)
}

pub(super) fn read_run_snapshots_at(
    project: &Path,
    session_id: &str,
) -> Result<Vec<WorkflowRun>, String> {
    let dir = project.join(session_id).join("workflows");

    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        // A session that ran no workflow legitimately has no directory.
        Err(_) => return Ok(Vec::new()),
    };

    let mut runs: Vec<(u64, WorkflowRun)> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();

        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }

        let Some(snapshot) = read_json(&path) else {
            continue;
        };

        let Some(run) = restore_run(&snapshot) else {
            continue;
        };

        runs.push((snapshot["startTime"].as_u64().unwrap_or(0), run));
    }

    runs.sort_by_key(|(started_at, _)| *started_at);

    let mut restored: Vec<WorkflowRun> = runs.into_iter().map(|(_, run)| run).collect();

    let recorded: HashSet<String> = restored
        .iter()
        .filter_map(|run| run.run_id.clone())
        .collect();

    restored.extend(restore_interrupted_runs(
        &project.join(session_id),
        &recorded,
    ));

    Ok(restored)
}

/// Runs whose completion snapshot never landed. The snapshot is written after
/// a run ends, so a run the process outlived leaves only the directory it was
/// writing into — and those are exactly the long runs worth reopening.
///
/// What survives is the run's own name, the agents it started, and their
/// conversations. Phases, ordering, and per-agent accounting live in the
/// snapshot alone, so a run restored this way reports what it has and omits
/// the rest rather than inventing it.
fn restore_interrupted_runs(session: &Path, recorded: &HashSet<String>) -> Vec<WorkflowRun> {
    let Ok(entries) = fs::read_dir(session.join("subagents").join("workflows")) else {
        return Vec::new();
    };

    let mut runs = Vec::new();

    for entry in entries.flatten() {
        let dir = entry.path();

        let Some(run_id) = dir
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
        else {
            continue;
        };

        if !dir.is_dir() || recorded.contains(&run_id) {
            continue;
        }

        let agents = restore_interrupted_agents(&dir);

        if agents.is_empty() {
            continue;
        }

        runs.push(WorkflowRun {
            // With no snapshot there is no stream task id to key on. The
            // directory id is unique and cannot collide with a live run's,
            // so it stands in as this run's identity.
            task_id: run_id.clone(),
            name: interrupted_run_name(session, &run_id),
            run_id: Some(run_id),
            summary: None,
            state: WorkflowRunState::Stopped,
            phases: Vec::new(),
            agents,
            total_tokens: None,
            total_tool_calls: None,
            result: None,
            refresh_failed: false,
        });
    }

    runs
}

/// Agents of an interrupted run, oldest transcript first. The journal records
/// only the agents whose results were cached, so the transcripts are what says
/// which agents actually ran; the journal then settles which of them finished.
fn restore_interrupted_agents(dir: &Path) -> Vec<WorkflowAgent> {
    let finished: HashSet<String> = read_journal(dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry.result.is_some())
        .map(|entry| entry.agent_id)
        .collect();

    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut found: Vec<(SystemTime, String, PathBuf)> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();

        let Some(agent_id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("agent-"))
            .and_then(|name| name.strip_suffix(".jsonl"))
            .map(str::to_owned)
        else {
            continue;
        };

        let started_at = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        found.push((started_at, agent_id, path));
    }

    found.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

    found
        .into_iter()
        .enumerate()
        .map(|(index, (_, agent_id, path))| WorkflowAgent {
            index: index as u64 + 1,
            label: agent_prompt_label(&path),
            state: if finished.contains(&agent_id) {
                WorkflowAgentState::Done
            } else {
                WorkflowAgentState::Stopped
            },
            agent_id: Some(agent_id),
            phase_index: None,
            phase_title: None,
            agent_type: None,
            isolation: None,
            model: None,
            tokens: None,
            tool_calls: None,
            reused: false,
            error: None,
            prompt_preview: None,
            result_preview: None,
        })
        .collect()
}

/// The workflow's name, from the script copy the provider keeps beside the
/// session. The file is `<name>-<run-id>.js`, so the name is its stem.
fn interrupted_run_name(session: &Path, run_id: &str) -> Option<String> {
    let entries = fs::read_dir(session.join("workflows").join("scripts")).ok()?;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_str()?;

        if let Some(stem) = name
            .strip_suffix(".js")
            .and_then(|stem| stem.strip_suffix(run_id))
            .and_then(|stem| stem.strip_suffix('-'))
            && !stem.is_empty()
        {
            return Some(stem.to_owned());
        }
    }

    None
}

/// A label for an agent whose run recorded no metadata: the opening line of
/// the prompt it was given, which is what its author wrote to identify it.
fn agent_prompt_label(path: &Path) -> Option<String> {
    /// The prompt is the first user record; scanning a few lines covers a
    /// file that opens with attachments instead.
    const SCAN_LINES: usize = 8;

    const MAX_LABEL_CHARS: usize = 80;

    let file = fs::File::open(path).ok()?;

    for line in BufReader::new(file)
        .lines()
        .take(SCAN_LINES)
        .map_while(Result::ok)
    {
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };

        if record["type"].as_str() != Some("user") {
            continue;
        }

        let content = &record["message"]["content"];

        let text = content.as_str().map(str::to_owned).or_else(|| {
            content
                .as_array()?
                .iter()
                .find_map(|block| block["text"].as_str())
                .map(str::to_owned)
        })?;

        let heading = text
            .lines()
            .map(|line| line.trim().trim_start_matches('#').trim())
            .find(|line| !line.is_empty())?;

        return Some(match heading.char_indices().nth(MAX_LABEL_CHARS) {
            Some((cut, _)) => format!("{}…", &heading[..cut]),
            None => heading.to_owned(),
        });
    }

    None
}

fn restore_run(snapshot: &Value) -> Option<WorkflowRun> {
    let task_id = text_field(snapshot, &["taskId"])?;
    let (phases, agents) = parse_progress(&snapshot["workflowProgress"]);

    Some(WorkflowRun {
        task_id,
        run_id: text_field(snapshot, &["runId"]),
        name: text_field(snapshot, &["workflowName"]),
        summary: text_field(snapshot, &["summary"]),
        // A run recorded by a previous process cannot still be advancing,
        // so an unrecognized status settles as stopped rather than active.
        state: match snapshot["status"].as_str() {
            Some("completed") => WorkflowRunState::Done,
            Some("failed") => WorkflowRunState::Failed,
            _ => WorkflowRunState::Stopped,
        },
        phases,
        agents,
        total_tokens: snapshot["totalTokens"].as_u64(),
        total_tool_calls: snapshot["totalToolCalls"].as_u64(),
        result: restored_result(snapshot),
        refresh_failed: false,
    })
}

/// The run's own final text. It is recorded as an array of per-agent results,
/// so the parts are joined rather than showing only the first.
fn restored_result(snapshot: &Value) -> Option<String> {
    if let Some(text) = text_field(snapshot, &["result"]) {
        return Some(text);
    }

    let parts: Vec<String> = snapshot["result"]
        .as_array()?
        .iter()
        .filter_map(|part| {
            part.as_str()
                .map(str::to_owned)
                .or_else(|| (!part.is_null()).then(|| part.to_string()))
        })
        .collect();

    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}
