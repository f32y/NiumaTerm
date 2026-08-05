//! Claude Code session history: enumerate and replay the transcript files the
//! CLI persists under `~/.claude/projects/<munged-cwd>/<session-id>.jsonl`.
//!
//! The transcript format is an implementation detail of the CLI, so parsing
//! here depends on a minimal field set (`type`, `message.content`,
//! `isSidechain`, `isMeta`, `gitBranch`) and skips any line it does not
//! recognize — an unparseable session degrades to an id-prefix title instead
//! of failing the list.

use std::io::{BufRead, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::{env, fs};

use serde_json::Value;

use crate::chat::{ReplayItem, SessionSummary};
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
/// real prompt: sidechain (subagent) traffic, meta records, and tool-result
/// containers.
fn user_prompt_text(record: &Value) -> Option<String> {
    if record["type"].as_str() != Some("user")
        || record["isSidechain"].as_bool() == Some(true)
        || record["isMeta"].as_bool() == Some(true)
    {
        return None;
    }

    let content = &record["message"]["content"];

    // Content is either a plain string or an array of typed blocks.
    let text = match content {
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
pub fn load_replay(cwd: Option<&str>, session_id: &str) -> Vec<ReplayItem> {
    let Some(path) = project_dir(cwd).map(|dir| dir.join(format!("{session_id}.jsonl"))) else {
        return Vec::new();
    };
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };

    parse_replay(BufReader::new(file))
}

fn parse_replay(reader: impl BufRead) -> Vec<ReplayItem> {
    let mut items: Vec<ReplayItem> = Vec::new();
    let mut tools = 0usize;

    for line in reader.lines().map_while(Result::ok) {
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };

        match record["type"].as_str() {
            Some("user") => {
                let Some(text) = user_prompt_text(&record) else {
                    continue;
                };
                let text = clean_prompt(&text);

                if text.is_empty() {
                    continue;
                }

                flush_tools(&mut items, &mut tools);
                items.push(ReplayItem::User { text });
            }
            Some("assistant") => {
                if record["isSidechain"].as_bool() == Some(true) {
                    continue;
                }
                let Some(blocks) = record["message"]["content"].as_array() else {
                    continue;
                };

                for block in blocks {
                    match block["type"].as_str() {
                        Some("text") => {
                            let text = block["text"].as_str().unwrap_or_default().trim();

                            if !text.is_empty() {
                                flush_tools(&mut items, &mut tools);
                                items.push(ReplayItem::Agent {
                                    text: text.to_string(),
                                });
                            }
                        }
                        Some("tool_use") | Some("server_tool_use") | Some("mcp_tool_use") => {
                            tools += 1;
                        }
                        // Thinking blocks are working noise, not conversation.
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    flush_tools(&mut items, &mut tools);

    items
}

fn flush_tools(items: &mut Vec<ReplayItem>, tools: &mut usize) {
    if *tools > 0 {
        items.push(ReplayItem::Tools { count: *tools });
        *tools = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn replay_keeps_dialogue_and_collapses_tools() {
        let lines = [
            serde_json::json!({"type": "queue-operation", "operation": "enqueue"}),
            serde_json::json!({"type": "user",
                "message": {"content": [{"type": "text", "text": "question"}]}}),
            serde_json::json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {}},
                {"type": "tool_use", "id": "t2", "name": "Read", "input": {}}]}}),
            serde_json::json!({"type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "t1"}]}}),
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
                ReplayItem::User {
                    text: "question".into()
                },
                ReplayItem::Tools { count: 2 },
                ReplayItem::Agent {
                    text: "answer".into()
                },
            ]
        );
    }
}
