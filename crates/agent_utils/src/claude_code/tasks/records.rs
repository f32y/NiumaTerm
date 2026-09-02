//! Reading one Claude record: the identifiers it carries, the state it
//! reports, and the text worth showing from it.
//!
//! These take a record and answer a question about it, without touching the
//! reduction's own state, which is what lets the reducer above stay about
//! sequencing rather than about record shapes.

use serde_json::Value;

use crate::background_task::{BackgroundTaskRefs, BackgroundTaskState};
use crate::claude_code::tasks::{AGENT_TASK_TYPE, SHELL_TASK_TYPE};
use crate::json::{condense, text_field};

/// Whether a lifecycle record may create a row nothing is known about yet. A
/// child agent is admitted from its own type alone. A shell is admitted only
/// once it reports being backgrounded, because every `Bash` call registers one
/// and the foreground ones are already visible as the tool row that started
/// them.
pub(super) fn admits_new_row(task_type: Option<&str>, record: &Value) -> bool {
    match task_type {
        Some(AGENT_TASK_TYPE) => true,
        Some(SHELL_TASK_TYPE) => record["is_backgrounded"].as_bool().unwrap_or(false),
        _ => false,
    }
}

/// Every identifier a lifecycle record supplies. Their presence together in one
/// record is what makes them aliases of the same child.
pub(super) fn record_identifiers(record: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    for key in ["task_id", "tool_use_id", "agent_id"] {
        if let Some(id) = record[key].as_str().filter(|id| !id.is_empty())
            && !ids.iter().any(|known| known == id)
        {
            ids.push(id.to_owned());
        }
    }
    ids
}

/// The identifier `stop_task` accepts. The CLI registers a delegated agent
/// under its task id and an agent task's id is that same value, so either names
/// the child; a tool-use id belongs to the parent's tool call instead and would
/// not be found in the task registry.
pub(super) fn stop_target(refs: &BackgroundTaskRefs) -> Option<&str> {
    match refs {
        BackgroundTaskRefs::ClaudeCode {
            task_id, agent_id, ..
        } => task_id.as_deref().or(agent_id.as_deref()),
        BackgroundTaskRefs::Codex { .. } | BackgroundTaskRefs::DeepSeek { .. } => None,
    }
}

pub(super) fn refs_from(record: &Value) -> BackgroundTaskRefs {
    BackgroundTaskRefs::ClaudeCode {
        task_id: text_field(record, &["task_id"]),
        tool_use_id: text_field(record, &["tool_use_id"]),
        agent_id: text_field(record, &["agent_id"]),
    }
}

/// Lifecycle state a task record reports. `task_notification` carries only
/// terminal statuses; `task_updated` carries the full vocabulary and is the
/// only place a stopped task's `killed` reliably appears.
pub(super) fn lifecycle_state(kind: &str, record: &Value) -> Option<BackgroundTaskState> {
    let status = match kind {
        "task_started" => return Some(BackgroundTaskState::Working),
        "task_progress" => return Some(BackgroundTaskState::Working),
        "task_notification" => record["status"].as_str()?,
        "task_updated" => record["patch"]["status"]
            .as_str()
            .or_else(|| record["status"].as_str())?,
        _ => return None,
    };
    Some(match status {
        "pending" => BackgroundTaskState::Starting,
        "running" => BackgroundTaskState::Working,
        // A paused background task is waiting on something outside itself,
        // which is what the panel shows as needing input.
        "paused" => BackgroundTaskState::NeedsInput,
        "completed" => BackgroundTaskState::Done,
        "failed" => BackgroundTaskState::Failed,
        "stopped" | "killed" => BackgroundTaskState::Stopped,
        _ => return None,
    })
}

pub(super) fn result_text(block: &Value) -> Option<String> {
    condense(&result_content(block)?)
}

/// A tool result's text as the CLI wrote it. Kept apart from the condensed
/// preview a row shows, because a handoff result states a path that is longer
/// than the preview bound and would be cut in half by it.
pub(super) fn result_content(block: &Value) -> Option<String> {
    let content = &block["content"];
    content.as_str().map(str::to_owned).or_else(|| {
        let parts: Vec<&str> = content
            .as_array()?
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect();
        (!parts.is_empty()).then(|| parts.join("\n"))
    })
}

/// One-line summary of a child's latest sidechain record.
pub(super) fn sidechain_preview(message: &Value) -> Option<String> {
    for block in message["message"]["content"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let text = match block["type"].as_str() {
            Some("text") => block["text"].as_str(),
            Some("thinking") => block["thinking"].as_str(),
            Some("tool_use") => block["name"].as_str(),
            _ => None,
        };
        if let Some(condensed) = text.and_then(condense) {
            return Some(condensed);
        }
    }
    None
}
