//! Claude Code session history: enumerate and replay the transcript files the
//! CLI persists under `~/.claude/projects/<munged-cwd>/<session-id>.jsonl`.
//!
//! The transcript format is an implementation detail of the CLI, so parsing
//! here depends on a minimal field set (`type`, `subtype`, `message.content`,
//! tool block ids/names/inputs, `isSidechain`, `isMeta`, `isCompactSummary`,
//! `compactMetadata`, `uuid`, `gitBranch`) and skips any line it does not
//! recognize — an unparseable session degrades to an id-prefix title instead of
//! failing the list.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, BufWriter, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::{env, fs};

use chrono::{SecondsFormat, Utc};
use serde_json::Value;
use tracing::warn;
use uuid::Uuid;

use super::compaction::{compaction_metadata, parse_compaction};
use super::tool_items::{complete_tool_item, tool_item};
use crate::chat::{Compaction, Item, SessionSummary};
use crate::hook_store::home_dir;

const TRANSCRIPT_ENTRY_TYPES: [&str; 5] = ["user", "assistant", "progress", "system", "attachment"];

/// Whether the selected user message has a persisted file-history snapshot.
/// `Unknown` is reserved for snapshot records whose schema is not understood;
/// the provider remains the final authority when that happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileRestoreAvailability {
    Available,
    Unavailable,
    Unknown,
}

/// One human prompt that can serve as a Claude rewind target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeCheckpoint {
    pub user_message_id: String,
    pub parent_message_id: Option<String>,
    pub prompt: String,
    pub timestamp: Option<String>,
    pub file_restore_availability: FileRestoreAvailability,
}

/// A conversation rewind either starts a fresh process before the first
/// prompt or resumes an immutable prefix copied into a new Claude session.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaudeFork {
    pub session_id: Option<String>,
    pub replay: Vec<Item>,
}

/// Parsed transcript plus the single active parent chain selected using the
/// same leaf rules as the official Agent SDK session helpers.
struct TranscriptIndex {
    records: Vec<Value>,
    chain: Vec<usize>,
    snapshot_message_ids: HashSet<String>,
    malformed_snapshot: bool,
    broken_parent: Option<String>,
}

impl TranscriptIndex {
    fn read(reader: impl BufRead) -> Self {
        let records = reader
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
            .collect::<Vec<_>>();
        let (chain, broken_parent) = active_chain_indices(&records);
        let mut snapshot_message_ids = HashSet::new();
        let mut malformed_snapshot = false;

        for record in &records {
            if record["type"].as_str() != Some("file-history-snapshot") {
                continue;
            }

            match record["messageId"].as_str() {
                Some(message_id) => {
                    snapshot_message_ids.insert(message_id.to_string());
                }
                None => malformed_snapshot = true,
            }
        }

        Self {
            records,
            chain,
            snapshot_message_ids,
            malformed_snapshot,
            broken_parent,
        }
    }

    fn active_records(&self) -> impl Iterator<Item = &Value> {
        self.chain
            .iter()
            .filter_map(|index| self.records.get(*index))
    }

    fn checkpoints(&self) -> Vec<ClaudeCheckpoint> {
        let mut checkpoints = self
            .active_records()
            .filter_map(|record| {
                let user_message_id = record["uuid"].as_str()?.to_string();
                let prompt = clean_prompt(&user_prompt_text(record)?);

                if prompt.is_empty() {
                    return None;
                }

                let file_restore_availability =
                    if self.snapshot_message_ids.contains(&user_message_id) {
                        FileRestoreAvailability::Available
                    } else if self.malformed_snapshot {
                        FileRestoreAvailability::Unknown
                    } else {
                        FileRestoreAvailability::Unavailable
                    };

                Some(ClaudeCheckpoint {
                    user_message_id,
                    parent_message_id: record["parentUuid"].as_str().map(str::to_owned),
                    prompt,
                    timestamp: record["timestamp"].as_str().map(str::to_owned),
                    file_restore_availability,
                })
            })
            .collect::<Vec<_>>();

        checkpoints.reverse();
        checkpoints
    }
}

fn is_transcript_entry(record: &Value) -> bool {
    record["type"]
        .as_str()
        .is_some_and(|kind| TRANSCRIPT_ENTRY_TYPES.contains(&kind))
        && record["uuid"].as_str().is_some()
}

/// Find the latest main-conversation leaf, then walk only `parentUuid` back to
/// the root. `logicalParentUuid` crosses a compaction boundary into content the
/// summary replaced, so following it would duplicate discarded conversation.
fn active_chain_indices(records: &[Value]) -> (Vec<usize>, Option<String>) {
    let transcript = records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| is_transcript_entry(record).then_some(index))
        .collect::<Vec<_>>();

    if transcript.is_empty() {
        // Very old or synthetic fixtures can lack UUID links entirely. There is
        // no branch information to recover, so chronological replay is the only
        // lossless compatibility behavior available.
        return ((0..records.len()).collect(), None);
    }

    let mut by_uuid = HashMap::new();
    let mut parent_uuids = HashSet::new();

    for &index in &transcript {
        let record = &records[index];
        let uuid = record["uuid"].as_str().unwrap_or_default().to_string();
        by_uuid.insert(uuid, index);

        if let Some(parent) = record["parentUuid"].as_str() {
            parent_uuids.insert(parent.to_string());
        }
    }

    let terminals = transcript.iter().copied().filter(|index| {
        records[*index]["uuid"]
            .as_str()
            .is_some_and(|uuid| !parent_uuids.contains(uuid))
    });
    let mut leaves = Vec::new();

    for terminal in terminals {
        let mut current = Some(terminal);
        let mut seen = HashSet::new();

        while let Some(index) = current {
            let record = &records[index];
            let Some(uuid) = record["uuid"].as_str() else {
                break;
            };
            if !seen.insert(uuid.to_string()) {
                break;
            }
            if matches!(record["type"].as_str(), Some("user" | "assistant")) {
                leaves.push(index);
                break;
            }

            current = record["parentUuid"]
                .as_str()
                .and_then(|parent| by_uuid.get(parent).copied());
        }
    }

    let main_leaves = leaves.iter().copied().filter(|index| {
        let record = &records[*index];
        record["isSidechain"].as_bool() != Some(true)
            && record["isMeta"].as_bool() != Some(true)
            && record["teamName"].as_str().is_none()
    });
    let leaf = main_leaves.max().or_else(|| leaves.into_iter().max());
    let Some(mut current) = leaf else {
        return (Vec::new(), None);
    };

    let mut reversed = Vec::new();
    let mut seen = HashSet::new();
    let mut broken_parent = None;

    loop {
        let record = &records[current];
        let Some(uuid) = record["uuid"].as_str() else {
            break;
        };
        if !seen.insert(uuid.to_string()) {
            break;
        }
        reversed.push(current);

        let Some(parent) = record["parentUuid"].as_str() else {
            break;
        };
        let Some(parent_index) = by_uuid.get(parent).copied() else {
            broken_parent = Some(parent.to_string());
            break;
        };
        current = parent_index;
    }

    reversed.reverse();
    (reversed, broken_parent)
}

/// The CLI resolves `--resume` against the project directory derived from the
/// process cwd, so listing and resuming must use the same directory mapping:
/// every non-ASCII-alphanumeric character becomes `-`.
fn munge_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The transcript directory for `cwd` (falling back to the process cwd, which
/// is what a spawned `claude` without an explicit working directory uses).
fn project_dir(cwd: Option<&str>) -> Option<PathBuf> {
    let cwd = match cwd {
        Some(cwd) => cwd.to_string(),
        None => env::current_dir().ok()?.to_string_lossy().into_owned(),
    };

    Some(
        home_dir()?
            .join(".claude")
            .join("projects")
            .join(munge_cwd(&cwd)),
    )
}

/// Cheap first pass for the history UI: how many sessions exist, so the list
/// can reserve its final height (placeholder rows) before any transcript
/// head is parsed for titles.
pub fn count_sessions(cwd: Option<&str>) -> usize {
    let Some(dir) = project_dir(cwd) else {
        return 0;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };

    entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .count()
}

/// Sessions resumable from `cwd`, newest first. Title extraction reads only
/// the head of each file, so listing a directory of multi-megabyte
/// transcripts stays cheap; still meant for a background thread.
pub fn list_sessions(cwd: Option<&str>) -> Vec<SessionSummary> {
    let Some(dir) = project_dir(cwd) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut sessions: Vec<SessionSummary> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();

            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                return None;
            }

            let id = path.file_stem()?.to_str()?.to_string();
            let last_active = entry.metadata().ok()?.modified().ok()?;
            let (title, branch) = head_title(&path);

            Some(SessionSummary {
                title: title.unwrap_or_else(|| id.chars().take(8).collect()),
                id,
                branch,
                last_active,
            })
        })
        .collect();

    sessions.sort_by(|a, b| b.last_active.cmp(&a.last_active));

    sessions
}

/// Head window scanned for the first user prompt. Sessions can open with
/// kilobytes of hook output and queue records before the first prompt, but
/// they stay well under this; anything past it falls back to the id title.
const TITLE_SCAN_BYTES: u64 = 64 * 1024;

/// First user prompt (and its recorded git branch) from the head of a
/// transcript file.
fn head_title(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(file) = fs::File::open(path) else {
        return (None, None);
    };

    for line in BufReader::new(file.take(TITLE_SCAN_BYTES))
        .lines()
        .map_while(Result::ok)
    {
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(text) = user_prompt_text(&record) else {
            continue;
        };
        let Some(title) = title_line(&text) else {
            continue;
        };

        let branch = record["gitBranch"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned);

        return (Some(title), branch);
    }

    (None, None)
}

/// The prompt text of a `user` record, or `None` for records that carry no
/// real prompt: sidechain (subagent) traffic, meta records, compaction
/// summaries, and tool-result containers.
fn user_prompt_text(record: &Value) -> Option<String> {
    if record["type"].as_str() != Some("user")
        || record["isSidechain"].as_bool() == Some(true)
        || record["isMeta"].as_bool() == Some(true)
        // The CLI stores a compaction summary as a synthesized user turn. It is
        // machine-written continuation context, so treating it as a prompt would
        // title a session with it and replay it as something the user typed;
        // `compaction_summary_text` claims it for its own transcript row.
        || is_compaction_summary(record)
    {
        return None;
    }

    record_text(record)
}

/// A `user` record the CLI synthesized to carry a compaction summary rather
/// than to record something the user sent.
fn is_compaction_summary(record: &Value) -> bool {
    record["type"].as_str() == Some("user") && record["isCompactSummary"].as_bool() == Some(true)
}

/// The summary a compaction left behind, if this record is the one carrying it.
fn compaction_summary_text(record: &Value) -> Option<String> {
    is_compaction_summary(record)
        .then(|| record_text(record))
        .flatten()
}

/// Readable text of a message record. Content is either a plain string or an
/// array of typed blocks; `None` covers both an unknown shape and a record
/// whose text is blank.
fn record_text(record: &Value) -> Option<String> {
    let text = match &record["message"]["content"] {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => {
            let parts: Vec<&str> = blocks
                .iter()
                .filter(|block| block["type"].as_str() == Some("text"))
                .filter_map(|block| block["text"].as_str())
                .collect();

            parts.join("\n")
        }
        _ => return None,
    };

    (!text.trim().is_empty()).then_some(text)
}

/// Strip the wrappers the CLI stores around prompts (injected
/// `<system-reminder>` context, slash-command wrappers) down to what the
/// user actually typed.
fn clean_prompt(text: &str) -> String {
    let mut text = text.to_string();

    // Injected context blocks wrap or precede the real prompt.
    while let (Some(start), Some(end)) = (
        text.find("<system-reminder>"),
        text.find("</system-reminder>"),
    ) {
        if end < start {
            break;
        }
        text.replace_range(start..end + "</system-reminder>".len(), "");
    }

    // Slash commands are stored in tagged wrappers; the command message (or name)
    // is the readable form.
    for tag in ["command-message", "command-name"] {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");

        if let (Some(start), Some(end)) = (text.find(&open), text.find(&close)) {
            if start < end {
                return text[start + open.len()..end].trim().to_string();
            }
        }
    }

    text.trim().to_string()
}

/// One-line title from a prompt: cleaned, first non-empty line, capped.
fn title_line(text: &str) -> Option<String> {
    let cleaned = clean_prompt(text);
    let line = cleaned.lines().find(|line| !line.trim().is_empty())?.trim();

    let title: String = line.chars().take(120).collect();

    (!title.is_empty()).then_some(title)
}

/// Reconstruct a session's conversation for the transcript UI. Reads the
/// whole file (resume replays nothing from the backend, so this is the only
/// source); meant for a background thread.
pub fn load_replay(cwd: Option<&str>, session_id: &str) -> Vec<Item> {
    let Some(path) = session_path(cwd, session_id) else {
        return Vec::new();
    };
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };

    parse_replay(BufReader::new(file))
}

/// Rewindable human prompts from the current active branch, newest first.
/// Reading stays synchronous because callers already run session file work on
/// a background executor.
pub fn load_checkpoints(
    cwd: Option<&str>,
    session_id: &str,
) -> Result<Vec<ClaudeCheckpoint>, String> {
    let path = session_path(cwd, session_id)
        .ok_or_else(|| format!("Claude session {session_id} has no project directory"))?;
    let file = fs::File::open(&path)
        .map_err(|error| format!("could not read Claude session {session_id}: {error}"))?;
    let transcript = TranscriptIndex::read(BufReader::new(file));

    if let Some(parent) = &transcript.broken_parent {
        warn!(
            session_id,
            missing_parent = parent,
            "Claude transcript active chain ended at a missing parent"
        );
    }

    Ok(transcript.checkpoints())
}

/// Fork the active conversation immediately before a human prompt. The source
/// JSONL is never opened for writing, and file-history snapshots are not part
/// of the copied active chain, so the result has no inherited file undo state.
pub fn fork_session_before(
    cwd: Option<&str>,
    source_session_id: &str,
    user_message_id: &str,
) -> Result<ClaudeFork, String> {
    Uuid::parse_str(source_session_id)
        .map_err(|_| format!("invalid Claude session id: {source_session_id}"))?;
    Uuid::parse_str(user_message_id)
        .map_err(|_| format!("invalid Claude user message id: {user_message_id}"))?;

    let source_path = session_path(cwd, source_session_id)
        .ok_or_else(|| format!("Claude session {source_session_id} has no project directory"))?;
    let source = fs::File::open(&source_path)
        .map_err(|error| format!("could not read Claude session {source_session_id}: {error}"))?;
    let transcript = TranscriptIndex::read(BufReader::new(source));

    if let Some(parent) = &transcript.broken_parent {
        warn!(
            session_id = source_session_id,
            missing_parent = parent,
            "Claude transcript fork ended at a missing parent"
        );
    }

    let new_session_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let Some(records) = build_fork_records(
        &transcript,
        source_session_id,
        user_message_id,
        &new_session_id,
        &now,
    )?
    else {
        return Ok(ClaudeFork {
            session_id: None,
            replay: Vec::new(),
        });
    };

    let project_dir = source_path
        .parent()
        .ok_or_else(|| format!("Claude session {source_session_id} has no parent directory"))?;
    let target = write_fork_file(project_dir, &new_session_id, &records)?;
    let replay_file = fs::File::open(&target)
        .map_err(|error| format!("could not replay fork {new_session_id}: {error}"))?;

    Ok(ClaudeFork {
        session_id: Some(new_session_id),
        replay: parse_replay(BufReader::new(replay_file)),
    })
}

/// Build a prefix fork while preserving fields the current client does not
/// understand. UUID links are rewritten so the copied transcript is an
/// independent graph and can safely be resumed by Claude Code.
fn build_fork_records(
    transcript: &TranscriptIndex,
    source_session_id: &str,
    user_message_id: &str,
    new_session_id: &str,
    now: &str,
) -> Result<Option<Vec<Value>>, String> {
    let cutoff = transcript
        .chain
        .iter()
        .position(|index| {
            let record = &transcript.records[*index];
            record["uuid"].as_str() == Some(user_message_id) && user_prompt_text(record).is_some()
        })
        .ok_or_else(|| {
            format!(
                "Claude user message {user_message_id} is not a prompt on the active conversation"
            )
        })?;
    let prefix = transcript.chain[..cutoff]
        .iter()
        .filter_map(|index| transcript.records.get(*index))
        .collect::<Vec<_>>();

    if prefix.is_empty() {
        return Ok(None);
    }

    let uuid_mapping = prefix
        .iter()
        .filter_map(|record| {
            record["uuid"]
                .as_str()
                .map(|uuid| (uuid.to_string(), Uuid::new_v4().to_string()))
        })
        .collect::<HashMap<_, _>>();
    let by_uuid = prefix
        .iter()
        .filter_map(|record| {
            record["uuid"]
                .as_str()
                .map(|uuid| (uuid.to_string(), *record))
        })
        .collect::<HashMap<_, _>>();
    let writable = prefix
        .iter()
        .filter(|record| record["type"].as_str() != Some("progress"))
        .copied()
        .collect::<Vec<_>>();

    if writable.is_empty() {
        return Ok(None);
    }

    let mut records = Vec::with_capacity(writable.len() + 2);
    for (position, original) in writable.iter().enumerate() {
        let original_uuid = original["uuid"]
            .as_str()
            .ok_or_else(|| "Claude fork record is missing its UUID".to_string())?;
        let new_uuid = uuid_mapping
            .get(original_uuid)
            .ok_or_else(|| format!("Claude fork could not remap message {original_uuid}"))?;
        let mut parent = original["parentUuid"].as_str();
        let mut new_parent = None;

        while let Some(parent_uuid) = parent {
            let Some(parent_record) = by_uuid.get(parent_uuid) else {
                break;
            };
            if parent_record["type"].as_str() != Some("progress") {
                new_parent = uuid_mapping.get(parent_uuid).cloned();
                break;
            }
            parent = parent_record["parentUuid"].as_str();
        }

        let mut forked = (*original).clone();
        let object = forked
            .as_object_mut()
            .ok_or_else(|| "Claude fork record is not a JSON object".to_string())?;
        object.insert("uuid".into(), Value::String(new_uuid.clone()));
        object.insert(
            "parentUuid".into(),
            new_parent.map(Value::String).unwrap_or(Value::Null),
        );
        let logical_parent = original["logicalParentUuid"]
            .as_str()
            .and_then(|uuid| uuid_mapping.get(uuid))
            .cloned();
        object.insert(
            "logicalParentUuid".into(),
            logical_parent.map(Value::String).unwrap_or(Value::Null),
        );
        object.insert("sessionId".into(), Value::String(new_session_id.into()));
        object.insert("isSidechain".into(), Value::Bool(false));
        object.insert(
            "timestamp".into(),
            if position + 1 == writable.len() {
                Value::String(now.into())
            } else {
                original
                    .get("timestamp")
                    .cloned()
                    .unwrap_or_else(|| Value::String(now.into()))
            },
        );
        object.insert(
            "forkedFrom".into(),
            serde_json::json!({
                "sessionId": source_session_id,
                "messageUuid": original_uuid,
            }),
        );
        for field in ["teamName", "agentName", "slug", "sourceToolAssistantUUID"] {
            object.remove(field);
        }
        records.push(forked);
    }

    let replacements = transcript
        .records
        .iter()
        .filter(|record| {
            record["type"].as_str() == Some("content-replacement")
                && record["sessionId"].as_str() == Some(source_session_id)
        })
        .filter_map(|record| record["replacements"].as_array())
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    if !replacements.is_empty() {
        records.push(serde_json::json!({
            "type": "content-replacement",
            "sessionId": new_session_id,
            "replacements": replacements,
            "uuid": Uuid::new_v4().to_string(),
            "timestamp": now,
        }));
    }

    let title = prefix
        .iter()
        .find_map(|record| user_prompt_text(record).as_deref().and_then(title_line))
        .unwrap_or_else(|| "Rewound session".into());
    records.push(serde_json::json!({
        "type": "custom-title",
        "sessionId": new_session_id,
        "customTitle": format!("{title} (rewind)"),
        "uuid": Uuid::new_v4().to_string(),
        "timestamp": now,
    }));

    Ok(Some(records))
}

fn write_fork_file(
    project_dir: &Path,
    session_id: &str,
    records: &[Value],
) -> Result<PathBuf, String> {
    let target = project_dir.join(format!("{session_id}.jsonl"));
    let temp = project_dir.join(format!(".{session_id}.{}.tmp", Uuid::new_v4()));
    let write_result = (|| -> Result<(), String> {
        let file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .map_err(|error| format!("could not create Claude fork: {error}"))?;
        let mut writer = BufWriter::new(file);
        for record in records {
            serde_json::to_writer(&mut writer, record)
                .map_err(|error| format!("could not serialize Claude fork: {error}"))?;
            writer
                .write_all(b"\n")
                .map_err(|error| format!("could not write Claude fork: {error}"))?;
        }
        writer
            .flush()
            .map_err(|error| format!("could not flush Claude fork: {error}"))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|error| format!("could not sync Claude fork: {error}"))?;
        fs::rename(&temp, &target)
            .map_err(|error| format!("could not publish Claude fork: {error}"))
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    write_result.map(|()| target)
}

fn session_path(cwd: Option<&str>, session_id: &str) -> Option<PathBuf> {
    project_dir(cwd).map(|dir| dir.join(format!("{session_id}.jsonl")))
}

fn parse_replay(reader: impl BufRead) -> Vec<Item> {
    let transcript = TranscriptIndex::read(reader);

    if let Some(parent) = &transcript.broken_parent {
        warn!(
            missing_parent = parent,
            "Claude transcript replay ended at a missing parent"
        );
    }

    let mut items: Vec<Item> = Vec::new();
    let mut pending_tools: HashMap<String, usize> = HashMap::new();
    let mut message_seq = 0usize;
    let mut thinking_seq = 0usize;
    let mut compaction_seq = 0usize;
    // A compaction writes its summary first and its boundary marker second (the
    // marker's parent is the last summary message). The summary is the part
    // worth keeping, so it opens the row immediately and the marker enriches it,
    // which also leaves the row intact if the marker never made it to disk.
    let mut open_compaction: Option<usize> = None;

    for record in transcript.active_records() {
        if record["isSidechain"].as_bool() == Some(true) || record["isMeta"].as_bool() == Some(true)
        {
            continue;
        }

        match record["type"].as_str() {
            Some("user") => {
                complete_replayed_tools(&record, &mut items, &mut pending_tools);

                if let Some(summary) = compaction_summary_text(&record) {
                    compaction_seq += 1;
                    open_compaction = Some(items.len());
                    items.push(Item::Compaction {
                        id: replayed_compaction_id(&record, compaction_seq),
                        detail: Compaction {
                            summary: Some(summary),
                            ..Compaction::default()
                        },
                    });
                    continue;
                }

                if let Some(text) = user_prompt_text(&record) {
                    let text = clean_prompt(&text);
                    if !text.is_empty() {
                        items.push(Item::UserMessage { text: Some(text) });
                    }
                }
            }
            Some("system") if record["subtype"].as_str() == Some("compact_boundary") => {
                let detail = parse_compaction(compaction_metadata(&record));

                match open_compaction
                    .take()
                    .and_then(|index| items.get_mut(index))
                {
                    Some(Item::Compaction { id, detail: opened }) => {
                        // The boundary marker is the record the live protocol
                        // reports, so adopting its identity keeps a resumed row
                        // and a live one from being two separate entries.
                        *id = replayed_compaction_id(&record, compaction_seq);
                        *opened = Compaction {
                            summary: opened.summary.take(),
                            ..detail
                        };
                    }
                    // Some compaction paths preserve a message segment instead
                    // of writing a summary turn; the boundary still belongs in
                    // the transcript.
                    _ => {
                        compaction_seq += 1;
                        items.push(Item::Compaction {
                            id: replayed_compaction_id(&record, compaction_seq),
                            detail,
                        });
                    }
                }
            }
            Some("assistant") => {
                let Some(blocks) = record["message"]["content"].as_array() else {
                    continue;
                };
                let is_api_error = record["isApiErrorMessage"].as_bool() == Some(true);

                for block in blocks {
                    match block["type"].as_str() {
                        Some("text") => {
                            let text = block["text"].as_str().unwrap_or_default().trim();

                            if !text.is_empty() {
                                let item = if is_api_error {
                                    Item::Error {
                                        text: text.to_string(),
                                    }
                                } else {
                                    let id = format!("replay-message-{message_seq}");
                                    message_seq += 1;
                                    Item::AgentMessage {
                                        id,
                                        text: Some(text.to_string()),
                                    }
                                };
                                items.push(item);
                            }
                        }
                        Some("thinking") => {
                            let summary = block["thinking"].as_str().unwrap_or_default().trim();
                            if summary.is_empty() {
                                continue;
                            }

                            let id = block["id"].as_str().map(str::to_owned).unwrap_or_else(|| {
                                let id = format!("replay-thinking-{thinking_seq}");
                                thinking_seq += 1;
                                id
                            });
                            items.push(Item::Reasoning {
                                id,
                                summary: Some(summary.to_string()),
                            });
                        }
                        Some("tool_use") | Some("server_tool_use") | Some("mcp_tool_use") => {
                            let Some(id) = block["id"].as_str() else {
                                continue;
                            };
                            let item = tool_item(
                                id,
                                block["name"].as_str().unwrap_or("tool"),
                                &block["input"],
                            );
                            pending_tools.insert(id.to_string(), items.len());
                            items.push(item);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    items
}

/// Stable transcript id for a replayed compaction. The record's own uuid keeps
/// a resumed row identical to the one the live boundary event produces, so the
/// same compaction cannot end up with two entries.
fn replayed_compaction_id(record: &Value, sequence: usize) -> String {
    match record["uuid"].as_str() {
        Some(uuid) => format!("compaction-{uuid}"),
        None => format!("replay-compaction-{sequence}"),
    }
}

/// Historical `tool_use` and `tool_result` blocks live in separate JSONL
/// records. Updating the already-positioned replay item keeps transcript order
/// while adding the completion payload and status.
fn complete_replayed_tools(
    record: &Value,
    items: &mut [Item],
    pending_tools: &mut HashMap<String, usize>,
) {
    let Some(blocks) = record["message"]["content"].as_array() else {
        return;
    };

    for block in blocks {
        if block["type"].as_str() != Some("tool_result") {
            continue;
        }
        let Some(id) = block["tool_use_id"].as_str() else {
            continue;
        };
        let Some(index) = pending_tools.remove(id) else {
            continue;
        };
        let Some(item) = items.get_mut(index) else {
            continue;
        };

        *item = complete_tool_item(item.clone(), block);
    }
}

#[cfg(test)]
mod tests;
