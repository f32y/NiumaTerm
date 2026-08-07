//! Claude Code session history: enumerate and replay the transcript files the
//! CLI persists under `~/.claude/projects/<munged-cwd>/<session-id>.jsonl`.
//!
//! The transcript format is an implementation detail of the CLI, so parsing
//! here depends on a minimal field set (`type`, `subtype`, `message.content`,
//! tool block ids/names/inputs, `isSidechain`, `isMeta`, `isCompactSummary`,
//! `compactMetadata`, `uuid`, `gitBranch`) and skips any line it does not
//! recognize — an unparseable session degrades to an id-prefix title instead of
//! failing the list.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::{env, fs};

use serde_json::Value;

use super::compaction::{compaction_metadata, parse_compaction};
use super::tool_items::{complete_tool_item, tool_item};
use crate::chat::{Compaction, Item, SessionSummary};
use crate::hook_store::home_dir;

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
/// `<system-reminder>` context, slash-command envelopes) down to what the
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

    // Slash commands are stored as an envelope; the command message (or name)
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
/// whole file (resume replays nothing on the wire, so this is the only
/// source); meant for a background thread.
pub fn load_replay(cwd: Option<&str>, session_id: &str) -> Vec<Item> {
    let Some(path) = project_dir(cwd).map(|dir| dir.join(format!("{session_id}.jsonl"))) else {
        return Vec::new();
    };
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };

    parse_replay(BufReader::new(file))
}

fn parse_replay(reader: impl BufRead) -> Vec<Item> {
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

    for line in reader.lines().map_while(Result::ok) {
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };

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
mod tests {
    use super::*;
    use crate::chat::CompactionTrigger;

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
            Some("fix the login bug".to_string())
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

    #[test]
    fn a_compaction_replays_as_one_row_carrying_summary_and_accounting() {
        let lines = [
            serde_json::json!({"type": "user",
                "message": {"content": [{"type": "text", "text": "question"}]}}),
            serde_json::json!({"type": "user", "uuid": "summary-uuid",
                "isCompactSummary": true, "isVisibleInTranscriptOnly": true,
                "message": {"content": "## Summary\nwhat happened so far"}}),
            serde_json::json!({"type": "system", "subtype": "compact_boundary",
                "uuid": "boundary-uuid", "parentUuid": "summary-uuid", "isMeta": false,
                "content": "Conversation compacted",
                "compactMetadata": {"trigger": "auto", "preTokens": 154_000,
                    "postTokens": 32_000, "messagesSummarized": 87}}),
            serde_json::json!({"type": "assistant",
                "message": {"content": [{"type": "text", "text": "answer"}]}}),
        ];
        let content: Vec<String> = lines.iter().map(|l| l.to_string()).collect();

        let items = parse_replay(content.join("\n").as_bytes());

        assert_eq!(
            items,
            vec![
                Item::UserMessage {
                    text: Some("question".into())
                },
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

    #[test]
    fn a_boundary_without_a_summary_turn_still_marks_the_break() {
        let lines = [
            serde_json::json!({"type": "system", "subtype": "compact_boundary",
                "compactMetadata": {"trigger": "manual", "preTokens": 90_000}}),
            serde_json::json!({"type": "assistant",
                "message": {"content": [{"type": "text", "text": "after"}]}}),
        ];
        let content: Vec<String> = lines.iter().map(|l| l.to_string()).collect();

        let items = parse_replay(content.join("\n").as_bytes());

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

        let items = parse_replay(line.to_string().as_bytes());

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
                 "input": {"command": "cargo check"}},
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

        let items = parse_replay(content.join("\n").as_bytes());

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

        let items = parse_replay(line.to_string().as_bytes());

        assert_eq!(
            items,
            vec![Item::Error {
                text: "You've hit your session limit · resets 3:20pm (Asia/Shanghai)".into()
            }]
        );
    }
}
