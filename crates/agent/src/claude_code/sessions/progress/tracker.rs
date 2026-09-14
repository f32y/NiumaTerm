use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::progress::{GoalStatus, Task, TaskList, TaskStatus};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProgressSnapshot {
    pub(crate) goal: Option<GoalStatus>,
    pub(crate) tasks: TaskList,
}

#[derive(Default)]
pub(super) struct ProgressTracker {
    pub(super) snapshot: ProgressSnapshot,
    pending: HashMap<String, (String, Value)>,
}

impl ProgressTracker {
    pub(super) fn observe(&mut self, record: &Value) {
        if record["isSidechain"] == true || !record["parent_tool_use_id"].is_null() {
            return;
        }

        if record["type"] == "attachment" && record["attachment"]["type"] == "goal_status" {
            self.goal(&record["attachment"]);
        }

        for block in record["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
        {
            match block["type"].as_str() {
                Some("tool_use") => {
                    let Some(
                        name @ ("TodoWrite" | "TaskCreate" | "TaskUpdate" | "TaskList" | "TaskGet"),
                    ) = block["name"].as_str()
                    else {
                        continue;
                    };

                    if let Some(id) = block["id"].as_str() {
                        self.pending
                            .insert(id.to_owned(), (name.to_owned(), block["input"].clone()));
                    }
                }
                Some("tool_result") => {
                    let Some((name, input)) = block["tool_use_id"]
                        .as_str()
                        .and_then(|id| self.pending.remove(id))
                    else {
                        continue;
                    };

                    if block["is_error"] == true {
                        continue;
                    }

                    let result = record
                        .get("toolUseResult")
                        .or_else(|| record.get("tool_use_result"));

                    let text = block["content"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| {
                            block["content"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|part| part["text"].as_str())
                                .collect::<Vec<_>>()
                                .join("\n")
                        });

                    let parsed = serde_json::from_str::<Value>(&text).ok();

                    self.tool_result(
                        &name,
                        &input,
                        result.or(parsed.as_ref()).unwrap_or(&Value::Null),
                        &text,
                    );
                }
                _ => {}
            }
        }
    }

    fn goal(&mut self, value: &Value) {
        if value["sentinel"] == true && value["met"] == true {
            self.snapshot.goal = None;

            return;
        }

        let Some(objective) = value["condition"].as_str() else {
            return;
        };

        let previous = self
            .snapshot
            .goal
            .as_ref()
            .filter(|goal| goal.objective == objective);

        let rounds = value["iterations"].as_u64().unwrap_or_else(|| {
            if value["sentinel"] == true {
                0
            } else {
                previous
                    .map_or(0, |goal| goal.rounds_started)
                    .saturating_add(1)
            }
        });

        self.snapshot.goal = Some(GoalStatus {
            objective: objective.to_owned(),
            phase: if value["failed"] == true {
                "failed"
            } else if value["met"] == true {
                "complete"
            } else {
                "active"
            }
            .into(),
            reason: value["reason"].as_str().map(str::to_owned),
            rounds_started: rounds,
            tokens_used: value["tokens"].as_u64(),
            elapsed_seconds: value["durationMs"].as_u64().map(|ms| ms / 1000),
            ..GoalStatus::default()
        });
    }

    fn tool_result(&mut self, name: &str, input: &Value, result: &Value, text: &str) {
        match name {
            "TodoWrite" => {
                let Some(todos) = input["todos"].as_array() else {
                    return;
                };

                self.snapshot.tasks.items = todos
                    .iter()
                    .enumerate()
                    .filter_map(|(index, todo)| {
                        Some(Task {
                            id: index.to_string(),
                            title: todo["content"].as_str()?.to_owned(),
                            status: TaskStatus::parse(todo["status"].as_str()?)?,
                            description: None,
                            owner: None,
                            blocked_by: Vec::new(),
                        })
                    })
                    .collect();
            }
            "TaskCreate" => {
                let id = result["task"]["id"]
                    .as_str()
                    .or_else(|| result["taskId"].as_str())
                    .map(str::to_owned)
                    .or_else(|| {
                        text.strip_prefix("Task #")
                            .and_then(|text| text.split_once(" created successfully"))
                            .map(|(id, _)| id.to_owned())
                    });

                if let Some(id) = id {
                    self.upsert(&id, input)
                }
            }
            "TaskUpdate" => {
                let Some(id) = input["taskId"]
                    .as_str()
                    .or_else(|| input["id"].as_str())
                    .or_else(|| input["task_id"].as_str())
                else {
                    return;
                };

                if result["success"] == false {
                    return;
                }

                if input["status"] == "deleted" {
                    self.snapshot.tasks.items.retain(|task| task.id != id);
                } else {
                    self.upsert(id, input);
                }
            }
            "TaskList" => {
                if let Some(tasks) = result.as_array().or_else(|| result["tasks"].as_array()) {
                    let ids: Vec<_> = tasks
                        .iter()
                        .filter_map(|task| task["id"].as_str())
                        .collect();

                    self.snapshot
                        .tasks
                        .items
                        .retain(|task| ids.contains(&task.id.as_str()));

                    for task in tasks {
                        if let Some(id) = task["id"].as_str() {
                            self.upsert(id, task)
                        }
                    }
                }
            }
            "TaskGet" => {
                let task = result.get("task").unwrap_or(result);

                if let Some(id) = task["id"].as_str() {
                    self.upsert(id, task)
                }
            }
            _ => {}
        }
    }

    fn upsert(&mut self, id: &str, value: &Value) {
        let index = self
            .snapshot
            .tasks
            .items
            .iter()
            .position(|task| task.id == id);

        let task = match index {
            Some(index) => &mut self.snapshot.tasks.items[index],
            None => {
                let Some(title) = value["subject"].as_str() else {
                    return;
                };

                self.snapshot.tasks.items.push(Task {
                    id: id.to_owned(),
                    title: title.to_owned(),
                    description: None,
                    status: TaskStatus::Pending,
                    owner: None,
                    blocked_by: Vec::new(),
                });

                self.snapshot
                    .tasks
                    .items
                    .last_mut()
                    .expect("the new task was appended")
            }
        };

        if let Some(title) = value["subject"].as_str() {
            task.title = title.to_owned()
        }

        if let Some(description) = value["description"].as_str() {
            task.description = Some(description.to_owned())
        }

        if let Some(status) = value["status"].as_str().and_then(TaskStatus::parse) {
            task.status = status
        }

        if let Some(owner) = value["owner"].as_str() {
            task.owner = (!owner.is_empty()).then(|| owner.to_owned())
        }

        if let Some(blocked) = value["blockedBy"].as_array() {
            task.blocked_by = blocked
                .iter()
                .filter_map(|id| id.as_str().map(str::to_owned))
                .collect();
        }

        for id in value["addBlockedBy"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !task.blocked_by.iter().any(|old| old == id) {
                task.blocked_by.push(id.to_owned())
            }
        }
    }
}
