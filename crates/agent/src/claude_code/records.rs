//! Readers of Claude records shared by the live stream-json protocol and
//! persisted session replay: compaction-boundary metadata and tool-call
//! items. Keeping both here means a resumed conversation decodes exactly what
//! a live one did.
//!
//! The two carry the same record under different key conventions: the SDK
//! output message uses `compact_metadata` with snake_case fields, while the
//! transcript file keeps the CLI's internal `compactMetadata` with camelCase
//! fields. Reading both spellings from one parser keeps a resumed boundary as
//! detailed as a live one.

#[cfg(test)]
#[path = "records_tests.rs"]
mod records_tests;

use std::collections::HashMap;

use serde_json::Value;

use crate::chat::{Compaction, CompactionTrigger, Item};
use crate::json::{block_text, diff_lines};

/// The metadata object of a `compact_boundary` record, whichever key
/// convention produced it.
pub(super) fn compaction_metadata(record: &Value) -> &Value {
    let snake = &record["compact_metadata"];

    if snake.is_object() {
        snake
    } else {
        &record["compactMetadata"]
    }
}

pub(super) fn parse_compaction(metadata: &Value) -> Compaction {
    Compaction {
        trigger: match metadata["trigger"].as_str() {
            Some("auto") => Some(CompactionTrigger::Automatic),
            Some("manual") => Some(CompactionTrigger::Manual),
            _ => None,
        },
        pre_tokens: token_count(metadata, "pre_tokens", "preTokens"),
        post_tokens: token_count(metadata, "post_tokens", "postTokens"),
        messages_summarized: token_count(metadata, "messages_summarized", "messagesSummarized"),
        user_context: text(metadata, "user_context", "userContext"),
        summary: None,
    }
}

fn token_count(metadata: &Value, snake: &str, camel: &str) -> Option<u64> {
    metadata[snake]
        .as_u64()
        .or_else(|| metadata[camel].as_u64())
}

fn text(metadata: &Value, snake: &str, camel: &str) -> Option<String> {
    metadata[snake]
        .as_str()
        .or_else(|| metadata[camel].as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

// Pure Claude tool-item mapping shared by live stream events and persisted
// session replay. Keeping protocol interpretation here prevents restored
// cards from losing fields when the live path learns a new tool shape.

/// Map a tool-use block to a transcript item: Bash becomes a command card,
/// file-editing tools become file-change cards, everything else a titled tool
/// card.
pub(crate) fn tool_item(id: &str, name: &str, input: &Value) -> Item {
    let id = id.to_string();
    let status = Some("inProgress".to_string());

    match name {
        "Bash" => Item::CommandExecution {
            id,
            command: input["command"].as_str().unwrap_or_default().to_string(),
            purpose: input["description"]
                .as_str()
                .filter(|description| !description.trim().is_empty())
                .map(str::to_owned),
            aggregated_output: None,
            status,
            exit_code: None,
        },
        "Edit" | "Write" | "NotebookEdit" => Item::FileChange {
            id,
            paths: input["file_path"]
                .as_str()
                .unwrap_or("(unknown file)")
                .to_string(),
            diff: edit_diff(name, input),
            status,
        },
        _ => Item::Other {
            id,
            kind: name.to_string(),
            title: tool_title(input),
            output: input_detail(name, input),
            status,
        },
    }
}

/// Merge a `tool_result` block into the item created from its matching
/// `tool_use`. Input detail for plans/todos survives acknowledgement, while
/// other tools expose their returned content.
pub(crate) fn complete_tool_item(started: Item, result: &Value) -> Item {
    let failed = result["is_error"].as_bool().unwrap_or(false);
    let status = Some(if failed { "failed" } else { "completed" }.to_string());
    let output = block_text(&result["content"], false).unwrap_or_default();

    match started {
        Item::CommandExecution {
            id,
            command,
            purpose,
            ..
        } => Item::CommandExecution {
            id,
            command,
            purpose,
            aggregated_output: Some(output),
            status,
            exit_code: None,
        },
        Item::FileChange {
            id, paths, diff, ..
        } => Item::FileChange {
            id,
            paths,
            diff,
            status,
        },
        Item::Other {
            id,
            kind,
            title,
            output: seeded,
            ..
        } => Item::Other {
            id,
            kind,
            title,
            output: seeded.or(Some(output)),
            status,
        },
        other => other,
    }
}

/// Detail seeded from the tool input when the request is more informative
/// than the result acknowledgement.
pub(super) fn input_detail(name: &str, input: &Value) -> Option<String> {
    match name {
        "TodoWrite" => input["todos"].as_array().map(|todos| {
            todos
                .iter()
                .filter_map(|todo| {
                    let content = todo["content"].as_str()?;

                    let mark = if todo["status"].as_str() == Some("completed") {
                        "x"
                    } else {
                        " "
                    };

                    Some(format!("- [{mark}] {content}"))
                })
                .collect::<Vec<_>>()
                .join("\n")
        }),
        "ExitPlanMode" => input["plan"].as_str().map(str::to_owned),
        _ => None,
    }
}

/// Reconstruct a reviewable +/- diff body from a file-editing tool's input.
pub(super) fn edit_diff(name: &str, input: &Value) -> Option<String> {
    let (removed, added) = match name {
        "Edit" => (
            input["old_string"].as_str().unwrap_or_default(),
            input["new_string"].as_str().unwrap_or_default(),
        ),
        "Write" => ("", input["content"].as_str().unwrap_or_default()),
        "NotebookEdit" => ("", input["new_source"].as_str().unwrap_or_default()),
        _ => return None,
    };

    if removed.is_empty() && added.is_empty() {
        return None;
    }

    Some(diff_lines(removed, added))
}

/// Best-effort one-line label for an arbitrary tool call, from the input
/// fields common across built-in and MCP tools.
pub(super) fn tool_title(input: &Value) -> String {
    for key in [
        "description",
        "file_path",
        "pattern",
        "query",
        "url",
        "path",
        "prompt",
        "skill",
    ] {
        if let Some(value) = input[key].as_str().filter(|s| !s.is_empty()) {
            return value.to_string();
        }
    }

    String::new()
}

/// Decode child content while retaining tool starts until their results arrive.
pub(crate) fn child_content_items(
    record: &Value,
    open_tools: &mut HashMap<String, Item>,
) -> Vec<Item> {
    let mut items = Vec::new();

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
                    questions: None,
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
