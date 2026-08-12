use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};

use serde_json::Value;
use tracing::warn;

use super::super::compaction::{compaction_metadata, parse_compaction};
use super::super::tool_items::{complete_tool_item, tool_item};
use super::ClaudeCheckpoint;
use super::index::TranscriptIndex;
use super::paths::session_path;
use super::titles::{clean_prompt, compaction_summary_text, user_prompt_text};
use crate::chat::{Compaction, Item};

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

pub(super) fn parse_replay(reader: impl BufRead) -> Vec<Item> {
    parse_transcript(reader, /*sidechain*/ false)
}

/// A child agent's own file holds nothing but sidechain records, so replaying
/// it keeps exactly the records the parent conversation drops. Both go through
/// one parser, which is what makes a child read identically to its parent.
pub(super) fn parse_child_replay(reader: impl BufRead) -> Vec<Item> {
    parse_transcript(reader, /*sidechain*/ true)
}

fn parse_transcript(reader: impl BufRead, sidechain: bool) -> Vec<Item> {
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
    // A compaction writes two records, a boundary marker carrying the token
    // accounting and a synthesized user turn carrying the summary, and their
    // order in the chain differs between CLI versions: current builds parent
    // the summary to the boundary, older ones parent the boundary to the
    // summary. Whichever arrives first opens one row and the other fills in its
    // half, so a compaction is one row either way, and a row whose other half
    // never reached the file still marks the break.
    let mut summary_awaiting_boundary: Option<usize> = None;
    let mut boundary_awaiting_summary: Option<usize> = None;

    for record in transcript.active_records() {
        if record["isSidechain"].as_bool().unwrap_or(false) != sidechain
            || record["isMeta"].as_bool() == Some(true)
        {
            continue;
        }

        match record["type"].as_str() {
            Some("user") => {
                complete_replayed_tools(&record, &mut items, &mut pending_tools);

                if let Some(summary) = compaction_summary_text(&record) {
                    match boundary_awaiting_summary
                        .take()
                        .and_then(|index| items.get_mut(index))
                    {
                        Some(Item::Compaction { detail, .. }) => detail.summary = Some(summary),
                        _ => {
                            compaction_seq += 1;
                            summary_awaiting_boundary = Some(items.len());
                            items.push(Item::Compaction {
                                id: replayed_compaction_id(&record, compaction_seq),
                                detail: Compaction {
                                    summary: Some(summary),
                                    ..Compaction::default()
                                },
                            });
                        }
                    }
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

                match summary_awaiting_boundary
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
                    // Either the summary turn follows the marker, or this
                    // compaction preserved a message segment instead of writing
                    // one at all; the boundary belongs in the transcript now and
                    // takes a summary later if one arrives.
                    _ => {
                        compaction_seq += 1;
                        boundary_awaiting_summary = Some(items.len());
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
