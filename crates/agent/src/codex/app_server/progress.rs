#[cfg(test)]
#[path = "progress_tests.rs"]
mod progress_tests;

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::codex::app_server::SessionDelivery;
use crate::progress::{GoalStatus, Task, TaskList, TaskStatus};

pub(super) const PLAN_RESTORED: &str = "nmt/codexPlanRestored";

pub(super) fn goal_status(value: &Value) -> Option<GoalStatus> {
    Some(GoalStatus {
        objective: value["objective"].as_str()?.to_owned(),
        phase: value["status"].as_str().unwrap_or_default().to_owned(),
        token_budget: value["tokenBudget"].as_u64(),
        tokens_used: value["tokensUsed"].as_u64(),
        elapsed_seconds: value["timeUsedSeconds"].as_u64(),
        ..GoalStatus::default()
    })
}

pub(super) fn task_list(value: &Value) -> TaskList {
    TaskList {
        explanation: value["explanation"].as_str().map(str::to_owned),
        items: value["plan"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(index, item)| {
                Some(Task::indexed(
                    index,
                    item["step"].as_str()?,
                    TaskStatus::parse(item["status"].as_str()?)?,
                ))
            })
            .collect(),
    }
}

/// The resume response omits checklist updates, but the local log retains them.
pub(super) fn read_plan(path: &Path) -> Option<Value> {
    let file = File::open(path).ok()?;

    let mut plan = None;

    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };

        let payload = &record["payload"];

        if record["type"] == "event_msg" && payload["type"] == "plan_update" {
            plan = Some(payload.clone());
        }
    }

    plan
}

/// Read the plan recorded in the thread log at `path` on the blocking pool and
/// deliver it as a `PLAN_RESTORED` notification. The log can be large, and
/// `thread_id` with `revision` let the receiver drop a plan that a newer
/// thread switch or checklist update has already superseded.
pub(super) fn spawn_plan_restore(
    path: PathBuf,
    thread_id: Option<String>,
    revision: u64,
    deliver: SessionDelivery,
) {
    nmt_platform::runtime().spawn_blocking(move || {
        deliver(json!({"method": PLAN_RESTORED, "params": {
            "threadId": thread_id, "revision": revision, "value": read_plan(&path)
        }}));
    });
}

pub(super) fn goal_request(id: u64, thread_id: &str, arguments: &str) -> Value {
    let arguments = arguments.trim();

    let mut params = json!({"threadId": thread_id});

    let method = match arguments {
        "" => "thread/goal/get",
        "clear" => "thread/goal/clear",
        "pause" | "resume" => {
            params["status"] = json!(if arguments == "pause" {
                "paused"
            } else {
                "active"
            });

            "thread/goal/set"
        }
        objective => {
            params["objective"] = json!(objective);
            params["status"] = json!("active");

            "thread/goal/set"
        }
    };

    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}
