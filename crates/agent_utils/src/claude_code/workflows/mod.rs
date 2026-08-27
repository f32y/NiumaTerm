//! Claude Code Dynamic Workflow reduction for the `Workflows` view.
//!
//! A workflow reaches the stream as a single task of type `local_workflow`
//! whose agents are entries in a `workflow_progress` array, never as child
//! agents of their own. The child-agent reducer in `claude_code::tasks`
//! deliberately rejects that task type, so this module reads the same records
//! for the other view rather than widening that one.
//!
//! Only `task_started` carries `task_type`; the `task_progress` records that
//! follow identify their run by `task_id` alone, so a run must be remembered
//! from its start for its own updates to be recognized.
use std::collections::HashMap;

use serde_json::Value;

use crate::json::text_field;
use crate::workflow::{
    WorkflowAgent, WorkflowAgentState, WorkflowPhase, WorkflowRun, WorkflowRunState,
    WorkflowSnapshot,
};

mod disk;

pub use crate::claude_code::workflows::disk::{
    RestoredWorkflowRun, WorkflowJournalEntry, WorkflowRefreshRequest, WorkflowRefreshResult,
    WorkflowTranscriptRead, agent_transcript_len, read_agent_transcript, read_journal,
    read_run_snapshots, refresh_run, resolve_run_directory,
};

/// Reduces the Claude stream into workflow runs. Mirrors the shape of the
/// child-agent reducer so both are driven from the same place.
#[derive(Default)]
pub(crate) struct ClaudeWorkflows {
    session_id: Option<String>,
    /// Runs by `task_id`, in first-seen order via `order`.
    runs: HashMap<String, WorkflowRun>,
    order: Vec<String>,
}

impl ClaudeWorkflows {
    pub(crate) fn snapshot(&self) -> Option<WorkflowSnapshot> {
        let session_id = self.session_id.clone()?;
        let runs = self
            .order
            .iter()
            .filter_map(|task_id| self.runs.get(task_id).cloned())
            .collect();

        Some(WorkflowSnapshot { session_id, runs })
    }

    /// Point the reducer at a session. A different id belongs to another
    /// conversation, so its runs are dropped.
    pub(crate) fn set_session(&mut self, session_id: &str) -> bool {
        if self.session_id.as_deref() == Some(session_id) {
            return false;
        }
        self.session_id = Some(session_id.to_owned());
        self.runs.clear();
        self.order.clear();
        true
    }

    /// Observe one incoming message. Returns true when run state changed.
    pub(crate) fn observe(&mut self, message: &Value) -> bool {
        if let Some(session_id) = message["session_id"].as_str() {
            self.set_session(session_id);
        }
        if self.session_id.is_none() || message["type"].as_str() != Some("system") {
            return false;
        }

        let subtype = message["subtype"].as_str().unwrap_or_default();
        let Some(task_id) = message["task_id"].as_str().filter(|id| !id.is_empty()) else {
            return false;
        };

        match subtype {
            "task_started" => {
                // The only record carrying the type, so it is the only place a
                // run can be admitted.
                if message["task_type"].as_str() != Some("local_workflow") {
                    return false;
                }
                self.start_run(task_id, message)
            }
            "task_progress" | "task_updated" | "task_notification" => {
                // These omit `task_type` entirely; a run must already be known.
                if !self.runs.contains_key(task_id) {
                    return false;
                }
                self.update_run(task_id, subtype, message)
            }
            _ => false,
        }
    }

    fn start_run(&mut self, task_id: &str, record: &Value) -> bool {
        if self.runs.contains_key(task_id) {
            return false;
        }
        self.order.push(task_id.to_owned());
        self.runs.insert(
            task_id.to_owned(),
            WorkflowRun {
                task_id: task_id.to_owned(),
                run_id: None,
                name: text_field(record, &["workflow_name"]),
                summary: text_field(record, &["description"]),
                state: WorkflowRunState::Starting,
                phases: Vec::new(),
                agents: Vec::new(),
                total_tokens: None,
                total_tool_calls: None,
                result: None,
                refresh_failed: false,
            },
        );
        true
    }

    fn update_run(&mut self, task_id: &str, subtype: &str, record: &Value) -> bool {
        let state = run_state(subtype, record);
        let progress = (!record["workflow_progress"].is_null())
            .then(|| parse_progress(&record["workflow_progress"]));
        let total_tokens = record["usage"]["total_tokens"].as_u64();
        let total_tool_calls = record["usage"]["tool_uses"].as_u64();
        let summary = text_field(record, &["summary"]);

        let Some(run) = self.runs.get_mut(task_id) else {
            return false;
        };
        let mut changed = false;

        if let Some((phases, agents)) = progress {
            // The provider repeats the whole array, so it replaces rather than
            // merges; a row that vanished from it is genuinely gone.
            if run.phases != phases {
                run.phases = phases;
                changed = true;
            }
            if run.agents != agents {
                run.agents = agents;
                changed = true;
            }
        }
        if let Some(state) = state
            && run.state != state
        {
            run.state = state;
            changed = true;
        }
        changed |= replace_number(&mut run.total_tokens, total_tokens);
        changed |= replace_number(&mut run.total_tool_calls, total_tool_calls);
        // A terminal record's summary is the run's own final text; earlier ones
        // describe the run rather than its outcome.
        if run.state.is_terminal() {
            changed |= replace_text(&mut run.result, &summary);
        } else {
            changed |= replace_text(&mut run.summary, &summary);
        }

        changed
    }

    /// Record what a disk refresh learned about one run.
    pub(crate) fn apply_refresh(&mut self, task_id: &str, refresh: WorkflowRefresh) -> bool {
        let Some(run) = self.runs.get_mut(task_id) else {
            return false;
        };
        let mut changed = false;

        if run.refresh_failed != refresh.failed {
            run.refresh_failed = refresh.failed;
            changed = true;
        }
        if let Some(run_id) = refresh.run_id
            && run.run_id.as_deref() != Some(run_id.as_str())
        {
            run.run_id = Some(run_id);
            changed = true;
        }

        // The journal reports an agent's own completion, which can land while
        // the stream is quiet; it never walks a row backwards.
        for entry in refresh.journal {
            let Some(agent) = run
                .agents
                .iter_mut()
                .find(|agent| agent.agent_id.as_deref() == Some(entry.agent_id.as_str()))
            else {
                continue;
            };
            let observed = match entry.result {
                Some(_) => WorkflowAgentState::Done,
                None => WorkflowAgentState::Running,
            };
            if agent.state == WorkflowAgentState::Queued
                || (agent.state == WorkflowAgentState::Running
                    && observed == WorkflowAgentState::Done)
            {
                agent.state = observed;
                changed = true;
            }
            if let Some(result) = entry.result {
                changed |= replace_text(&mut agent.result_preview, &Some(result));
            }
        }

        changed
    }

    /// Fold restored runs of a resumed session in. A run the live stream has
    /// already reported keeps its live state.
    pub(crate) fn merge_restored(&mut self, restored: Vec<RestoredWorkflowRun>) -> bool {
        let mut changed = false;
        for run in restored {
            if self.runs.contains_key(&run.run.task_id) {
                continue;
            }
            self.order.push(run.run.task_id.clone());
            self.runs.insert(run.run.task_id.clone(), run.run);
            changed = true;
        }
        changed
    }
}

/// What one disk refresh learned. Kept separate from the run so a failed read
/// cannot erase provider-reported state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkflowRefresh {
    pub run_id: Option<String>,
    pub journal: Vec<WorkflowJournalEntry>,
    pub failed: bool,
}

/// Run lifecycle a record reports. A start or progress record means only
/// that the run is live; the status vocabulary appears on the other two.
fn run_state(subtype: &str, record: &Value) -> Option<WorkflowRunState> {
    let status = match subtype {
        "task_progress" => return Some(WorkflowRunState::Running),
        "task_notification" => record["status"].as_str()?,
        "task_updated" => record["patch"]["status"]
            .as_str()
            .or_else(|| record["status"].as_str())?,
        _ => return None,
    };
    Some(match status {
        "pending" => WorkflowRunState::Starting,
        "running" => WorkflowRunState::Running,
        "completed" => WorkflowRunState::Done,
        "failed" => WorkflowRunState::Failed,
        "stopped" | "killed" => WorkflowRunState::Stopped,
        _ => return None,
    })
}

/// Split a `workflow_progress` array into the phases it declares and the
/// agent rows beneath them.
pub(crate) fn parse_progress(progress: &Value) -> (Vec<WorkflowPhase>, Vec<WorkflowAgent>) {
    let mut phases = Vec::new();
    let mut agents = Vec::new();

    for entry in progress.as_array().into_iter().flatten() {
        match entry["type"].as_str() {
            Some("workflow_phase") => {
                let Some(index) = entry["index"].as_u64() else {
                    continue;
                };
                phases.push(WorkflowPhase {
                    index,
                    title: entry["title"].as_str().unwrap_or_default().to_owned(),
                });
            }
            Some("workflow_agent") => {
                let Some(index) = entry["index"].as_u64() else {
                    continue;
                };
                agents.push(WorkflowAgent {
                    index,
                    agent_id: text_field(entry, &["agentId"]),
                    label: text_field(entry, &["label"]),
                    phase_index: entry["phaseIndex"].as_u64(),
                    phase_title: text_field(entry, &["phaseTitle"]),
                    agent_type: text_field(entry, &["agentType"]),
                    isolation: text_field(entry, &["isolation"]),
                    model: text_field(entry, &["model"]),
                    state: agent_state(entry),
                    tokens: entry["tokens"].as_u64(),
                    tool_calls: entry["toolCalls"].as_u64(),
                    reused: entry["cached"].as_bool().unwrap_or(false),
                    error: text_field(entry, &["error"]),
                    prompt_preview: text_field(entry, &["promptPreview"]),
                    result_preview: text_field(entry, &["resultPreview"]),
                });
            }
            _ => {}
        }
    }

    phases.sort_by_key(|phase| phase.index);
    agents.sort_by_key(|agent| agent.index);
    (phases, agents)
}

/// The provider reports `start`, `done`, or `error`. A started agent that has
/// not been picked up yet carries a queue time without a start time, which is
/// what separates Queued from Running.
fn agent_state(entry: &Value) -> WorkflowAgentState {
    match entry["state"].as_str() {
        Some("done") => WorkflowAgentState::Done,
        Some("error") => WorkflowAgentState::Failed,
        _ if entry["startedAt"].as_u64().is_some() => WorkflowAgentState::Running,
        _ => WorkflowAgentState::Queued,
    }
}

fn replace_text(current: &mut Option<String>, incoming: &Option<String>) -> bool {
    let Some(incoming) = incoming else {
        return false;
    };
    if current.as_deref() == Some(incoming.as_str()) {
        return false;
    }
    *current = Some(incoming.clone());
    true
}

fn replace_number(current: &mut Option<u64>, incoming: Option<u64>) -> bool {
    let Some(incoming) = incoming else {
        return false;
    };
    if *current == Some(incoming) {
        return false;
    }
    *current = Some(incoming);
    true
}

#[cfg(test)]
mod tests;
