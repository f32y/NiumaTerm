use std::fs;
use std::io::{BufRead, BufReader, Read as _};
use std::path::Path;

use serde_json::Value;

use crate::chat::SessionSummary;
use crate::claude_code::sessions::paths::project_dir;

/// Head window scanned for the first user prompt. Sessions can open with
/// kilobytes of hook output and queue records before the first prompt, but
/// they stay well under this; anything past it falls back to the id title.
const TITLE_SCAN_BYTES: u64 = 64 * 1024;

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
pub(super) fn user_prompt_text(record: &Value) -> Option<String> {
    if record["isSidechain"].as_bool() == Some(true) {
        return None;
    }

    conversation_user_text(record)
}

/// User text that belongs to the selected conversation. Replay selects parent
/// or child records before calling this helper, while session titles reject
/// child traffic through `user_prompt_text`.
pub(super) fn conversation_user_text(record: &Value) -> Option<String> {
    if record["type"].as_str() != Some("user")
        || record["isMeta"].as_bool() == Some(true)
        // The CLI stores a compaction summary as a synthesized user turn. It is
        // machine-written continuation context, so treating it as a prompt would
        // title a session with it and replay it as something the user typed;
        // `compaction_summary_text` claims it for its own transcript row.
        || is_compaction_summary(record)
        || is_task_notification(record)
        || is_interruption(record)
    {
        return None;
    }

    record_text(record)
}

/// A `user` record the CLI wrote to mark where the user stopped a running
/// turn. Its body is the fixed `[Request interrupted by user]` notice
/// addressed to the model, so replaying it as a prompt puts words in the
/// user's mouth and titles a session with them.
pub(super) fn is_interruption(record: &Value) -> bool {
    record["type"].as_str() == Some("user") && !record["interruptedMessageId"].is_null()
}

/// A `user` record the CLI synthesized to report a background agent's state.
/// It reads as plumbing addressed to the model — task and tool-use ids, an
/// output-file path, a status — so replaying it as a prompt shows the user
/// something they never typed.
fn is_task_notification(record: &Value) -> bool {
    match record["origin"]["kind"].as_str() {
        Some(kind) => kind == "task-notification",
        // Older CLI versions recorded no origin, leaving the notification
        // block itself as the only marker.
        None => record_text(record)
            .is_some_and(|text| text.trim_start().starts_with("<task-notification>")),
    }
}

/// A `user` record the CLI synthesized to carry a compaction summary rather
/// than to record something the user sent.
fn is_compaction_summary(record: &Value) -> bool {
    record["type"].as_str() == Some("user") && record["isCompactSummary"].as_bool() == Some(true)
}

/// The summary a compaction left behind, if this record is the one carrying it.
pub(super) fn compaction_summary_text(record: &Value) -> Option<String> {
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
pub(super) fn clean_prompt(text: &str) -> String {
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
pub(super) fn title_line(text: &str) -> Option<String> {
    let cleaned = clean_prompt(text);
    let line = cleaned.lines().find(|line| !line.trim().is_empty())?.trim();

    let title: String = line.chars().take(120).collect();

    (!title.is_empty()).then_some(title)
}
