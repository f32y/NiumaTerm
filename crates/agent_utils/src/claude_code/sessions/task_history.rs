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
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::DateTime;
use serde_json::Value;

use crate::background_task::{BackgroundTaskRefs, BackgroundTaskState, BackgroundTaskUpdate};
use crate::chat::Item;
use crate::claude_code::sessions::index::TranscriptIndex;
use crate::claude_code::sessions::paths::project_dir;
use crate::claude_code::sessions::replay::parse_child_replay;
use crate::claude_code::sessions::titles::conversation_user_text;
use crate::claude_code::tool_items::{complete_tool_item, tool_item};

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
    /// The child's own conversation, rebuilt from the sidechain records that
    /// link to its launch. Empty when the history holds only the launch.
    pub items: Vec<Item>,
}

/// Read one session's child agents. Meant for a background thread; the parent
/// transcript and composer never depend on this succeeding.
///
/// A conversation that has not written its transcript yet restores nothing
/// rather than failing: the CLI creates the file as the first turn produces
/// records, so a fresh session legitimately has no history to read.
pub fn load_task_history(cwd: Option<&str>, session_id: &str) -> Result<Vec<RestoredTask>, String> {
    let project = project_dir(cwd)
        .ok_or_else(|| format!("Claude session {session_id} has no project directory"))?;
    load_task_history_at(&project, session_id)
}

pub(super) fn load_task_history_at(
    project: &Path,
    session_id: &str,
) -> Result<Vec<RestoredTask>, String> {
    let mut tasks = load_launches(project, session_id)?;
    attach_child_transcripts(project, session_id, &mut tasks);
    Ok(tasks)
}

/// One child's conversation, read from the file the CLI wrote for it. Recent
/// versions publish a child's own turns only there: the parent stream carries
/// the launch instruction and lifecycle records but none of the child's
/// replies, so a live row has nothing to show without this read.
///
/// Returns `None` when this session kept no child files, which leaves whatever
/// the stream did supply in place rather than blanking it.
pub fn load_child_transcript(
    cwd: Option<&str>,
    session_id: &str,
    tool_use_id: &str,
) -> Option<Vec<Item>> {
    let project = project_dir(cwd)?;
    load_child_transcript_at(&project, session_id, tool_use_id)
}

pub(super) fn load_child_transcript_at(
    project: &Path,
    session_id: &str,
    tool_use_id: &str,
) -> Option<Vec<Item>> {
    let (_, conversation) = child_conversations(project, session_id)
        .into_iter()
        .find(|(meta, _)| meta["toolUseId"].as_str() == Some(tool_use_id))?;
    let file = fs::File::open(&conversation).ok()?;

    Some(parse_child_replay(BufReader::new(file)))
}

/// Every child of one session, as its metadata paired with the conversation
/// file beside it. `agent-<id>.meta.json` names `agent-<id>.jsonl`.
fn child_conversations(project: &Path, session_id: &str) -> Vec<(Value, PathBuf)> {
    let dir = project.join(session_id).join("subagents");
    let Ok(entries) = fs::read_dir(&dir) else {
        // No child ever ran, or this Claude version keeps them elsewhere.
        return Vec::new();
    };

    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                return None;
            }
            let meta = fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())?;

            Some((meta, path.with_extension("").with_extension("jsonl")))
        })
        .collect()
}

/// Fold each persisted child conversation onto the launch that started it.
/// A conversation whose metadata names no launch in this history belongs to
/// another branch and is left alone.
fn attach_child_transcripts(project: &Path, session_id: &str, tasks: &mut [RestoredTask]) {
    for (meta, conversation) in child_conversations(project, session_id) {
        let Some(tool_use_id) = meta["toolUseId"].as_str() else {
            continue;
        };
        let Some(task) = tasks.iter_mut().find(|task| task.id == tool_use_id) else {
            continue;
        };

        if let Ok(file) = fs::File::open(&conversation) {
            task.items = parse_child_replay(BufReader::new(file));
        }
        if task.update.agent_type.is_none() {
            task.update.agent_type = text_field(&meta, &["agentType"]);
        }
        if task.update.display_name.is_none() {
            task.update.display_name = text_field(&meta, &["description"]);
        }
        if task.update.depth.is_none() {
            task.update.depth = meta["spawnDepth"]
                .as_u64()
                .and_then(|d| u32::try_from(d).ok());
        }
    }
}

fn load_launches(project: &Path, session_id: &str) -> Result<Vec<RestoredTask>, String> {
    let path = project.join(format!("{session_id}.jsonl"));
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
    let mut open_tools: HashMap<String, Item> = HashMap::new();
    for record in &transcript.records {
        if is_process_boundary(record) {
            process_restarted = true;
            continue;
        }
        if let Some(parent) = linked_parent(record) {
            enrich_from_sidechain(parent, record, &mut tasks, &index, &mut open_tools);
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
            items: Vec::new(),
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
    open_tools: &mut HashMap<String, Item>,
) {
    let Some(position) = index.get(parent_tool_use_id) else {
        return;
    };
    let task = &mut tasks[*position];
    task.items.extend(child_items(record, open_tools));

    let Some(preview) = sidechain_preview(record) else {
        return;
    };
    task.update.status = Some(preview.clone());
    task.update.last_preview = Some(preview);
    if let Some(observed) = timestamp(record) {
        task.update.updated_at = Some(observed);
    }
}

/// Transcript items for one persisted sidechain record, using the same item
/// shapes the parent conversation renders.
fn child_items(record: &Value, open_tools: &mut HashMap<String, Item>) -> Vec<Item> {
    let mut items = Vec::new();
    if let Some(text) = conversation_user_text(record) {
        items.push(Item::UserMessage { text: Some(text) });
    }
    for block in record["message"]["content"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let Some(id) = block["id"]
            .as_str()
            .or_else(|| block["tool_use_id"].as_str())
            .map(str::to_owned)
            .or_else(|| record["uuid"].as_str().map(str::to_owned))
        else {
            continue;
        };
        match block["type"].as_str() {
            Some("text") if record["type"].as_str() != Some("user") => {
                items.push(Item::AgentMessage {
                    id,
                    text: block["text"].as_str().map(str::to_owned),
                })
            }
            Some("text") => {}
            Some("thinking") => items.push(Item::Reasoning {
                id,
                summary: block["thinking"].as_str().map(str::to_owned),
            }),
            Some("tool_use") => {
                let item = tool_item(
                    &id,
                    block["name"].as_str().unwrap_or("tool"),
                    &block["input"],
                );
                open_tools.insert(id, item.clone());
                items.push(item);
            }
            Some("tool_result") => {
                if let Some(started) = open_tools.remove(&id) {
                    items.push(complete_tool_item(started, block));
                }
            }
            _ => {}
        }
    }
    items
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
