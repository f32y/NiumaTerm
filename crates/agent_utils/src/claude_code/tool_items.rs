//! Pure Claude tool-item mapping shared by live stream events and persisted
//! session replay. Keeping protocol interpretation here prevents restored
//! cards from losing fields when the live path learns a new tool shape.

use serde_json::Value;

use crate::chat::Item;

/// Map a tool-use block to a transcript item: Bash becomes a command card,
/// file-editing tools become file-change cards, everything else a titled tool
/// card.
pub(super) fn tool_item(id: &str, name: &str, input: &Value) -> Item {
    let id = id.to_string();
    let status = Some("inProgress".to_string());

    match name {
        "Bash" => Item::CommandExecution {
            id,
            command: input["command"].as_str().unwrap_or_default().to_string(),
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
pub(super) fn complete_tool_item(started: Item, result: &Value) -> Item {
    let failed = result["is_error"].as_bool().unwrap_or(false);
    let status = Some(if failed { "failed" } else { "completed" }.to_string());
    let output = tool_result_text(&result["content"]);

    match started {
        Item::CommandExecution { id, command, .. } => Item::CommandExecution {
            id,
            command,
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

    let mut diff = String::new();
    for line in removed.lines() {
        diff.push('-');
        diff.push_str(line);
        diff.push('\n');
    }
    for line in added.lines() {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    Some(diff)
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

/// Extract readable text from a tool-result payload, which is either a plain
/// string or an array of content blocks.
fn tool_result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}
