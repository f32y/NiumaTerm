//! Workflow runs a conversation's tool started.
//!
//! Unlike the disk-backed producer, these arrive as ordinary session events, so
//! this is an accumulator over the log rather than a reader of a stored record:
//! each event carries only its own increment, and the run is what they add up
//! to. That is also why nothing here polls — a live run reports itself.

use serde_json::Value;

use crate::workflow::{
    WorkflowAgent, WorkflowAgentState, WorkflowPhase, WorkflowRun, WorkflowRunState,
    WorkflowSnapshot,
};

/// The runs seen so far, in the order they started.
#[derive(Default)]
pub(crate) struct WorkflowTracker {
    runs: Vec<WorkflowRun>,
}

impl WorkflowTracker {
    /// Fold one session event. Returns whether it changed anything, so a caller
    /// republishes the snapshot only when there is something new to show.
    pub(crate) fn apply(&mut self, event: &Value) -> bool {
        let data = &event["data"];
        let Some(run_id) = data["runId"].as_str() else {
            return false;
        };

        match event["type"].as_str() {
            Some("tool-workflow/run-start") => {
                // A run id is stable, and a log can be read more than once.
                if self.run(run_id).is_some() {
                    return false;
                }
                self.runs.push(WorkflowRun {
                    task_id: run_id.to_string(),
                    // The identity that names a stored record belongs to the
                    // disk-backed producer; here the run id is the only one.
                    run_id: None,
                    name: data["name"].as_str().map(str::to_string),
                    summary: None,
                    state: WorkflowRunState::Starting,
                    phases: Vec::new(),
                    agents: Vec::new(),
                    total_tokens: None,
                    total_tool_calls: None,
                    result: None,
                    refresh_failed: false,
                });
                true
            }
            Some("tool-workflow/agent-start") => self.start_agent(run_id, data),
            Some("tool-workflow/agent-end") => {
                let Some(seq) = data["seq"].as_u64() else {
                    return false;
                };
                let state = match data["outcome"].as_str() {
                    Some("completed") => WorkflowAgentState::Done,
                    Some("failed") => WorkflowAgentState::Failed,
                    // A cancelled member ended because the run did, which is
                    // the state a stopped member already means.
                    _ => WorkflowAgentState::Stopped,
                };
                let Some(agent) = self
                    .run_mut(run_id)
                    .and_then(|run| run.agents.iter_mut().find(|agent| agent.index == seq))
                else {
                    return false;
                };
                agent.state = state;
                true
            }
            Some("tool-workflow/run-end") => {
                let state = match data["stopReason"].as_str() {
                    Some("completed") => WorkflowRunState::Done,
                    Some("error") => WorkflowRunState::Failed,
                    _ => WorkflowRunState::Stopped,
                };
                let Some(run) = self.run_mut(run_id) else {
                    return false;
                };
                run.state = state;
                // A member the run outlived reports no ending of its own, so
                // its row would otherwise stay Running under a finished run.
                for agent in &mut run.agents {
                    if agent.state == WorkflowAgentState::Running {
                        agent.state = WorkflowAgentState::Stopped;
                    }
                }
                true
            }
            _ => false,
        }
    }

    pub(crate) fn snapshot(&self, session_id: &str) -> WorkflowSnapshot {
        WorkflowSnapshot {
            session_id: session_id.to_string(),
            runs: self.runs.clone(),
        }
    }

    fn start_agent(&mut self, run_id: &str, data: &Value) -> bool {
        let Some(seq) = data["seq"].as_u64() else {
            return false;
        };
        // A member names its group by title alone, so the run's list of groups
        // is built in the order members first mention one.
        let phase = data["phase"].as_str().map(str::to_string);
        let Some(run) = self.run_mut(run_id) else {
            return false;
        };
        if run.agents.iter().any(|agent| agent.index == seq) {
            return false;
        }

        let phase_index = phase.as_ref().map(|title| {
            match run.phases.iter().find(|entry| &entry.title == title) {
                Some(entry) => entry.index,
                None => {
                    let index = run.phases.len() as u64;
                    run.phases.push(WorkflowPhase {
                        index,
                        title: title.clone(),
                    });
                    index
                }
            }
        });

        run.agents.push(WorkflowAgent {
            index: seq,
            // The member's own session is what a later read would address, and
            // it is the only identity linking a row to a conversation.
            agent_id: data["childId"].as_str().map(str::to_string),
            label: data["label"].as_str().map(str::to_string),
            phase_index,
            phase_title: phase,
            agent_type: None,
            isolation: None,
            model: None,
            // A member is published only once its session exists, so it is
            // already under way when this arrives.
            state: WorkflowAgentState::Running,
            tokens: None,
            tool_calls: None,
            reused: false,
            error: None,
            prompt_preview: None,
            result_preview: None,
        });
        // The first member is what turns a declared run into a running one.
        run.state = WorkflowRunState::Running;
        true
    }

    fn run(&self, run_id: &str) -> Option<&WorkflowRun> {
        self.runs.iter().find(|run| run.task_id == run_id)
    }

    fn run_mut(&mut self, run_id: &str) -> Option<&mut WorkflowRun> {
        self.runs.iter_mut().find(|run| run.task_id == run_id)
    }
}
