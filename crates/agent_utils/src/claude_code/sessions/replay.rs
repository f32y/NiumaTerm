use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};

use chrono::DateTime;
use serde_json::Value;
use tracing::warn;

use crate::chat::{Compaction, Item, ReplayItem, ReplayTurn};
use crate::claude_code::compaction::{compaction_metadata, parse_compaction};
use crate::claude_code::sessions::ClaudeCheckpoint;
use crate::claude_code::sessions::index::TranscriptIndex;
use crate::claude_code::sessions::paths::session_path;
use crate::claude_code::sessions::titles::{
    clean_prompt, compaction_summary_text, conversation_user_text, is_interruption,
};
use crate::claude_code::tool_items::{complete_tool_item, tool_item};

/// Reconstruct a session's conversation for the transcript UI. Reads the
/// whole file (resume replays nothing from the backend, so this is the only
/// source); meant for a background thread.
pub fn load_replay(cwd: Option<&str>, session_id: &str) -> Vec<ReplayTurn> {
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

pub(super) fn parse_replay(reader: impl BufRead) -> Vec<ReplayTurn> {
    parse_transcript(reader, /*sidechain*/ false)
}

/// A child agent's own file holds nothing but sidechain records, so replaying
/// it keeps exactly the records the parent conversation drops. Both go through
/// one parser, which is what makes a child read identically to its parent. A
/// child's conversation is presented as one stream, so its turns are flattened.
pub(super) fn parse_child_replay(reader: impl BufRead) -> Vec<Item> {
    parse_transcript(reader, /*sidechain*/ true)
        .into_iter()
        .flat_map(|turn| turn.items)
        .map(|entry| entry.item)
        .collect()
}

fn parse_transcript(reader: impl BufRead, sidechain: bool) -> Vec<ReplayTurn> {
    let transcript = TranscriptIndex::read(reader);

    if let Some(parent) = &transcript.broken_parent {
        warn!(
            missing_parent = parent,
            "Claude transcript replay ended at a missing parent"
        );
    }

    // A turn's duration is written as a `turn_duration` record hanging off the
    // turn's last message rather than as a link in the parent chain, so it is
    // collected up front and matched by that parent as the chain is walked.
    let durations = turn_durations(&transcript.records);

    let mut items: Vec<ReplayItem> = Vec::new();
    let mut turns: Vec<TurnBuilder> = vec![TurnBuilder::default()];
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

        let at = record_time(record);
        if let Some(turn) = turns.last_mut() {
            if let Some(uuid) = record["uuid"].as_str()
                && let Some(duration) = durations.get(uuid)
            {
                turn.seconds = Some(duration / 1000);
            }
            if is_interruption(record) {
                turn.interrupted = true;
            }
            turn.output_tokens = match (turn.output_tokens, output_tokens(record)) {
                (Some(total), Some(tokens)) => Some(total + tokens),
                (total, tokens) => total.or(tokens),
            };
        }

        match record["type"].as_str() {
            Some("user") => {
                complete_replayed_tools(&record, &mut items, &mut pending_tools);

                if let Some(summary) = compaction_summary_text(&record) {
                    match boundary_awaiting_summary
                        .take()
                        .and_then(|index| items.get_mut(index))
                    {
                        Some(ReplayItem {
                            item: Item::Compaction { detail, .. },
                            ..
                        }) => detail.summary = Some(summary),
                        _ => {
                            compaction_seq += 1;
                            summary_awaiting_boundary = Some(items.len());
                            items.push(ReplayItem {
                                at,
                                item: Item::Compaction {
                                    id: replayed_compaction_id(&record, compaction_seq),
                                    detail: Compaction {
                                        summary: Some(summary),
                                        ..Compaction::default()
                                    },
                                },
                            });
                        }
                    }
                    continue;
                }

                if let Some(text) = conversation_user_text(&record) {
                    let text = if sidechain {
                        text.trim().to_owned()
                    } else {
                        clean_prompt(&text)
                    };
                    if !text.is_empty() {
                        // A prompt opens a turn, the same boundary the live path
                        // draws when the user sends one.
                        if turns.last().is_some_and(|turn| turn.start < items.len()) {
                            turns.push(TurnBuilder {
                                start: items.len(),
                                ..TurnBuilder::default()
                            });
                        }
                        items.push(ReplayItem {
                            at,
                            item: Item::UserMessage { text: Some(text) },
                        });
                    }
                }
            }
            Some("system") if record["subtype"].as_str() == Some("compact_boundary") => {
                let detail = parse_compaction(compaction_metadata(&record));

                match summary_awaiting_boundary
                    .take()
                    .and_then(|index| items.get_mut(index))
                    .map(|entry| &mut entry.item)
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
                        items.push(ReplayItem {
                            at,
                            item: Item::Compaction {
                                id: replayed_compaction_id(&record, compaction_seq),
                                detail,
                            },
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
                                items.push(ReplayItem { item, at });
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
                            items.push(ReplayItem {
                                at,
                                item: Item::Reasoning {
                                    id,
                                    summary: Some(summary.to_string()),
                                },
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
                            items.push(ReplayItem { item, at });
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    slice_turns(items, turns)
}

/// Accumulates one turn's accounting while its items are still being appended
/// to the flat list. The list stays flat because replayed tool results are
/// completed by index long after their call was positioned.
#[derive(Default)]
struct TurnBuilder {
    start: usize,
    seconds: Option<u64>,
    output_tokens: Option<u64>,
    interrupted: bool,
}

fn slice_turns(items: Vec<ReplayItem>, turns: Vec<TurnBuilder>) -> Vec<ReplayTurn> {
    let mut items: Vec<Option<ReplayItem>> = items.into_iter().map(Some).collect();
    let mut replay = Vec::with_capacity(turns.len());

    for (index, turn) in turns.iter().enumerate() {
        let end = turns
            .get(index + 1)
            .map_or(items.len(), |next| next.start.min(items.len()));
        let Some(range) = items.get_mut(turn.start..end) else {
            continue;
        };
        let items: Vec<ReplayItem> = range.iter_mut().filter_map(Option::take).collect();

        if items.is_empty() {
            continue;
        }

        replay.push(ReplayTurn {
            items,
            seconds: turn.seconds,
            output_tokens: turn.output_tokens,
            interrupted: turn.interrupted,
        });
    }

    replay
}

/// `durationMs` by the uuid of the message each turn ended on. The record sits
/// beside the chain rather than in it, so the walk cannot pick it up in order.
fn turn_durations(records: &[Value]) -> HashMap<String, u64> {
    records
        .iter()
        .filter(|record| {
            record["type"].as_str() == Some("system")
                && record["subtype"].as_str() == Some("turn_duration")
        })
        .filter_map(|record| {
            let parent = record["parentUuid"].as_str()?.to_string();
            Some((parent, record["durationMs"].as_u64()?))
        })
        .collect()
}

/// Output tokens an assistant record reported for itself. Summed over a turn,
/// these are the count the live path receives as a running total.
fn output_tokens(record: &Value) -> Option<u64> {
    (record["type"].as_str() == Some("assistant"))
        .then(|| record["message"]["usage"]["output_tokens"].as_u64())
        .flatten()
}

/// Wall-clock time of a record as Unix seconds.
fn record_time(record: &Value) -> Option<i64> {
    let stamp = record["timestamp"].as_str()?;
    DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|time| time.timestamp())
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
    items: &mut [ReplayItem],
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
        let Some(entry) = items.get_mut(index) else {
            continue;
        };

        entry.item = complete_tool_item(entry.item.clone(), block);
    }
}
