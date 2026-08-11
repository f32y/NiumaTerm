//! Rebuild child-agent summaries from a persisted Claude Code session file.
//!
//! Claude replays nothing on resume, so the transcript JSONL is the only
//! record of children that ran before the tab reopened. This pass is separate
//! from conversation replay: it selects `Task`/`Agent` launches from the main
//! conversation, then enriches them with the sidechain branches those launches
//! own. A sidechain that no selected launch claims is left alone, because
//! abandoned branches and other conversations' children would otherwise appear
//! in the current session.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, ErrorKind};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::DateTime;
use serde_json::Value;

use crate::background_task::{BackgroundTaskRefs, BackgroundTaskState, BackgroundTaskUpdate};
use crate::claude_code::sessions::index::TranscriptIndex;
use crate::claude_code::sessions::paths::session_path;

/// Tool names whose launch creates a child agent.
const LAUNCH_TOOLS: [&str; 2] = ["Task", "Agent"];

/// System subtypes that report a task's lifecycle. A stopped task reports
/// `killed` only in a `task_updated` patch, so both terminal records matter.
const LIFECYCLE_RECORDS: [&str; 4] = [
    "task_started",
    "task_progress",
    "task_notification",
    "task_updated",
];

/// The task type of delegated agent work; shells, monitors, and workflows share
/// these records and must not become rows.
const AGENT_TASK_TYPE: &str = "local_agent";

/// One child agent rebuilt from history, keyed by the identity the live
/// reducer uses so the two merge into a single row.
#[derive(Clone, Debug, PartialEq)]
pub struct RestoredTask {
    pub id: String,
    pub update: BackgroundTaskUpdate,
}

/// Read one session's child agents. Meant for a background thread; the parent
/// transcript and composer never depend on this succeeding.
///
/// A conversation that has not written its transcript yet restores nothing
/// rather than failing: the CLI creates the file as the first turn produces
/// records, so a fresh session legitimately has no history to read.
pub fn load_task_history(cwd: Option<&str>, session_id: &str) -> Result<Vec<RestoredTask>, String> {
    let path = session_path(cwd, session_id)
        .ok_or_else(|| format!("Claude session {session_id} has no project directory"))?;
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "could not read Claude session {session_id}: {error}"
            ));
        }
    };

    Ok(parse_task_history(BufReader::new(file)))
}

pub(super) fn parse_task_history(reader: impl BufRead) -> Vec<RestoredTask> {
    let transcript = TranscriptIndex::read(reader);
    let mut tasks: Vec<RestoredTask> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();

    // The main conversation decides which children belong to this session.
    for record in transcript.active_records() {
        if record["isSidechain"].as_bool() == Some(true) {
            continue;
        }
        collect_launches(record, &mut tasks, &mut index);
        collect_results(record, &mut tasks, &index);
    }

    // Only now can sidechains and lifecycle records be attributed, because a
    // link is accepted only against a launch the pass above already selected.
    let mut process_restarted = false;
    for record in &transcript.records {
        if is_process_boundary(record) {
            process_restarted = true;
            continue;
        }
        if let Some(parent) = linked_parent(record) {
            enrich_from_sidechain(parent, record, &mut tasks, &index);
            continue;
        }
        collect_lifecycle(record, &mut tasks, &index);
    }

    if process_restarted {
        // A conversation that outlived its process cannot still be running the
        // children that process owned.
        for task in &mut tasks {
            if task.update.state.is_none_or(BackgroundTaskState::is_active) {
                task.update.state = Some(BackgroundTaskState::Stopped);
            }
        }
    }

    tasks
}

fn collect_launches(
    record: &Value,
    tasks: &mut Vec<RestoredTask>,
    index: &mut HashMap<String, usize>,
) {
    if record["type"].as_str() != Some("assistant") {
        return;
    }
    for block in record["message"]["content"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if block["type"].as_str() != Some("tool_use") {
            continue;
        }
        let Some(name) = block["name"]
            .as_str()
            .filter(|name| LAUNCH_TOOLS.contains(name))
        else {
            continue;
        };
        let Some(tool_use_id) = block["id"].as_str() else {
            continue;
        };
        if index.contains_key(tool_use_id) {
            continue;
        }

        let input = &block["input"];
        index.insert(tool_use_id.to_owned(), tasks.len());
        tasks.push(RestoredTask {
            id: tool_use_id.to_owned(),
            update: BackgroundTaskUpdate {
                refs: Some(BackgroundTaskRefs::ClaudeCode {
                    task_id: None,
                    tool_use_id: Some(tool_use_id.to_owned()),
                    agent_id: None,
                }),
                state: Some(BackgroundTaskState::Starting),
                display_name: text_field(input, &["description", "name", "title"]),
                agent_type: text_field(input, &["subagent_type", "agent_type", "agent"])
                    .or_else(|| Some(name.to_owned())),
                objective: text_field(input, &["prompt", "task", "instructions"]),
                model: text_field(input, &["model"]),
                started_at: timestamp(record),
                updated_at: timestamp(record),
                ..BackgroundTaskUpdate::default()
            },
        });
    }
}

fn collect_results(record: &Value, tasks: &mut [RestoredTask], index: &HashMap<String, usize>) {
    if record["type"].as_str() != Some("user") {
        return;
    }
    for block in record["message"]["content"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if block["type"].as_str() != Some("tool_result") {
            continue;
        }
        let Some(position) = block["tool_use_id"].as_str().and_then(|id| index.get(id)) else {
            continue;
        };
        let update = &mut tasks[*position].update;
        update.state = Some(if block["is_error"].as_bool().unwrap_or(false) {
            BackgroundTaskState::Failed
        } else {
            BackgroundTaskState::Done
        });
        update.completed_at = timestamp(record);
        update.updated_at = timestamp(record).or(update.updated_at);
    }
}

fn enrich_from_sidechain(
    parent_tool_use_id: &str,
    record: &Value,
    tasks: &mut [RestoredTask],
    index: &HashMap<String, usize>,
) {
    let Some(position) = index.get(parent_tool_use_id) else {
        return;
    };
    let Some(preview) = sidechain_preview(record) else {
        return;
    };
    let update = &mut tasks[*position].update;
    update.status = Some(preview.clone());
    update.last_preview = Some(preview);
    if let Some(observed) = timestamp(record) {
        update.updated_at = Some(observed);
    }
}

fn collect_lifecycle(record: &Value, tasks: &mut [RestoredTask], index: &HashMap<String, usize>) {
    let Some(kind) = record["subtype"]
        .as_str()
        .filter(|kind| LIFECYCLE_RECORDS.contains(kind))
    else {
        return;
    };
    if record["task_type"]
        .as_str()
        .is_some_and(|task_type| task_type != AGENT_TASK_TYPE)
    {
        return;
    }
    // History only enriches launches the main conversation already selected, so
    // a record naming an unknown task belongs to another branch.
    let Some(position) = record_identifiers(record)
        .iter()
        .find_map(|id| index.get(id.as_str()))
    else {
        return;
    };

    let update = &mut tasks[*position].update;
    if let Some(state) = lifecycle_state(kind, record) {
        update.state = Some(state);
        if state.is_terminal() {
            update.completed_at = timestamp(record).or(update.completed_at);
        }
    }
    if let Some(status) = text_field(record, &["summary", "last_tool_name"]) {
        update.status = Some(status);
    }
    if let Some(observed) = timestamp(record) {
        update.updated_at = Some(observed);
    }
}

/// A record that starts a new CLI process for the same conversation.
fn is_process_boundary(record: &Value) -> bool {
    record["type"].as_str() == Some("system")
        && matches!(record["subtype"].as_str(), Some("init" | "session_start"))
}

/// The launch a sidechain record belongs to. Records without this link cannot
/// be attributed to any selected launch.
fn linked_parent(record: &Value) -> Option<&str> {
    record["parent_tool_use_id"]
        .as_str()
        .or_else(|| record["parentToolUseID"].as_str())
        .filter(|id| !id.is_empty())
}

fn record_identifiers(record: &Value) -> Vec<String> {
    ["task_id", "tool_use_id", "agent_id"]
        .iter()
        .filter_map(|key| record[*key].as_str().filter(|id| !id.is_empty()))
        .map(str::to_owned)
        .collect()
}

fn lifecycle_state(kind: &str, record: &Value) -> Option<BackgroundTaskState> {
    let status = match kind {
        "task_started" | "task_progress" => return Some(BackgroundTaskState::Working),
        "task_notification" => record["status"].as_str()?,
        "task_updated" => record["patch"]["status"]
            .as_str()
            .or_else(|| record["status"].as_str())?,
        _ => return None,
    };
    Some(match status {
        "pending" => BackgroundTaskState::Starting,
        "running" => BackgroundTaskState::Working,
        "paused" => BackgroundTaskState::NeedsInput,
        "completed" => BackgroundTaskState::Done,
        "failed" => BackgroundTaskState::Failed,
        "stopped" | "killed" => BackgroundTaskState::Stopped,
        _ => return None,
    })
}

fn sidechain_preview(record: &Value) -> Option<String> {
    for block in record["message"]["content"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let text = match block["type"].as_str() {
            Some("text") => block["text"].as_str(),
            Some("thinking") => block["thinking"].as_str(),
            Some("tool_use") => block["name"].as_str(),
            _ => None,
        };
        if let Some(condensed) = text.and_then(condense) {
            return Some(condensed);
        }
    }
    None
}

fn condense(text: &str) -> Option<String> {
    const MAX_PREVIEW_CHARS: usize = 160;
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }
    Some(match text.char_indices().nth(MAX_PREVIEW_CHARS) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text,
    })
}

/// Transcript records carry RFC 3339 timestamps. A record without a usable one
/// still restores its row; only the elapsed and completion labels are lost.
fn timestamp(record: &Value) -> Option<SystemTime> {
    let raw = record["timestamp"].as_str()?;
    let parsed = DateTime::parse_from_rfc3339(raw).ok()?;
    let seconds = u64::try_from(parsed.timestamp()).ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(seconds))
}

fn text_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value[*key]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    })
}
