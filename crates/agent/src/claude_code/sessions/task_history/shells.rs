use std::collections::HashMap;

use serde_json::{Value, json};

use crate::background_task::{
    BackgroundTaskKind, BackgroundTaskRefs, BackgroundTaskState, BackgroundTaskUpdate,
};
use crate::claude_code::sessions::task_history::{RestoredTask, timestamp};
use crate::claude_code::tasks::{handoff_output_file, lifecycle_state};
use crate::json::{block_text, text_field};

/// A background task id in a Bash result confirms that the command ran in the
/// background, including commands moved there after their initial timeout.
pub(super) fn collect_shells<'a>(records: impl Iterator<Item = &'a Value>) -> Vec<RestoredTask> {
    let mut launches = HashMap::new();
    let mut tasks = Vec::new();
    let mut index = HashMap::new();

    for record in records {
        if record["isSidechain"].as_bool() == Some(true) {
            continue;
        }

        let content = &record["message"]["content"];

        match record["type"].as_str() {
            Some("assistant") => {
                for block in content.as_array().into_iter().flatten() {
                    if block["type"] == "tool_use"
                        && block["name"] == "Bash"
                        && let Some(id) = block["id"].as_str()
                    {
                        launches.insert(id, (&block["input"], timestamp(record)));
                    }
                }
            }
            Some("user") => {
                let metadata = record
                    .get("toolUseResult")
                    .or_else(|| record.get("tool_use_result"));

                if let Some(task_id) = metadata
                    .and_then(|value| value["backgroundTaskId"].as_str())
                    .filter(|id| !id.is_empty())
                {
                    for block in content.as_array().into_iter().flatten() {
                        if block["type"] != "tool_result" {
                            continue;
                        }

                        let Some(tool_id) = block["tool_use_id"].as_str() else {
                            continue;
                        };

                        let Some((input, started_at)) = launches.get(tool_id) else {
                            continue;
                        };

                        if index.contains_key(task_id) {
                            continue;
                        }

                        index.insert(task_id.to_owned(), tasks.len());

                        tasks.push(RestoredTask {
                            id: task_id.to_owned(),
                            update: BackgroundTaskUpdate {
                                kind: Some(BackgroundTaskKind::Shell),
                                refs: Some(BackgroundTaskRefs::ClaudeCode {
                                    task_id: Some(task_id.to_owned()),
                                    tool_use_id: Some(tool_id.to_owned()),
                                    agent_id: None,
                                }),
                                state: Some(BackgroundTaskState::Stopped),
                                objective: text_field(input, &["command"]),
                                display_name: text_field(input, &["description"]),
                                started_at: *started_at,
                                ..BackgroundTaskUpdate::default()
                            },
                            items: Vec::new(),
                            output_file: block_text(&block["content"], false)
                                .and_then(|text| handoff_output_file(&text, task_id)),
                        });
                    }
                }

                if let Some(text) = block_text(content, true) {
                    apply_notification(&text, record, &mut tasks, &index);
                }
            }
            _ => {}
        }
    }

    tasks
}

fn apply_notification(
    text: &str,
    record: &Value,
    tasks: &mut [RestoredTask],
    index: &HashMap<String, usize>,
) {
    let Some(body) = text
        .trim()
        .strip_prefix("<task-notification>")
        .and_then(|text| text.strip_suffix("</task-notification>"))
    else {
        return;
    };

    let Some(position) = tag_value(body, "task-id").and_then(|id| index.get(id)) else {
        return;
    };

    let task = &mut tasks[*position];
    let status = json!({"status": tag_value(body, "status")});

    if let Some(state) = lifecycle_state("task_notification", &status) {
        task.update.state = Some(state);

        if state.is_terminal() {
            task.update.completed_at = timestamp(record);
        }
    }

    task.update.status = tag_value(body, "summary").map(str::to_owned);

    if let Some(path) = tag_value(body, "output-file") {
        task.output_file = Some(path.to_owned());
    }
}

fn tag_value<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let start = text.find(&format!("<{name}>"))? + name.len() + 2;
    let value = text[start..].split_once(&format!("</{name}>"))?.0.trim();

    (!value.is_empty()).then_some(value)
}
