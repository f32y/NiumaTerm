//! Backend-neutral model for a Dynamic Workflow run: the phases it moves
//! through, the agents it fans out to, and their progress.
//!
//! Providers supply progress and conversations through this shared vocabulary.

use crate::chat::Item;

/// Lifecycle of a whole run, as the view groups it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowRunState {
    Starting,
    Running,
    Done,
    Failed,
    Stopped,
}

impl WorkflowRunState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Stopped)
    }
}

/// Lifecycle of one agent within a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowAgentState {
    Queued,
    Running,
    Done,
    Failed,
    /// The run ended before this agent did. Only a restored run reports it:
    /// a live agent always resolves to one of the states above.
    Stopped,
}

/// One of the phases a run declares, in provider order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowPhase {
    pub index: u64,
    pub title: String,
}

/// One agent row. Every provider-sourced detail is optional because the
/// progress array is provider internals: a field the provider stops sending
/// must degrade the row rather than break the run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowAgent {
    /// Provider order within the run; also the row's stable identity when no
    /// agent id has been assigned yet.
    pub index: u64,

    /// Provider identity linking this row to its conversation.
    pub agent_id: Option<String>,

    pub label: Option<String>,
    pub phase_index: Option<u64>,
    pub phase_title: Option<String>,
    pub agent_type: Option<String>,
    pub isolation: Option<String>,
    pub model: Option<String>,
    pub state: WorkflowAgentState,
    pub tokens: Option<u64>,
    pub tool_calls: Option<u64>,

    /// The provider served this agent from an earlier run instead of running
    /// it again.
    pub reused: bool,

    pub error: Option<String>,
    pub prompt_preview: Option<String>,
    pub result_preview: Option<String>,
}

/// One workflow run as the view shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowRun {
    /// Stream identity, and the only identity a live run has.
    pub task_id: String,

    /// Provider run identity, when distinct from its live task identity.
    pub run_id: Option<String>,

    pub name: Option<String>,
    pub summary: Option<String>,
    pub state: WorkflowRunState,
    pub phases: Vec<WorkflowPhase>,
    pub agents: Vec<WorkflowAgent>,
    pub total_tokens: Option<u64>,
    pub total_tool_calls: Option<u64>,

    /// The run's own final text, once it has one.
    pub result: Option<String>,

    /// Set when a source read failed. It reports a refresh problem and never
    /// means the run itself failed.
    pub refresh_failed: bool,
}

impl WorkflowRun {
    /// Name to show when the provider supplied none.
    pub fn display_label(&self) -> String {
        match self.name.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => name.to_owned(),
            _ => format!("Workflow {}", self.task_id),
        }
    }

    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    pub fn agent(&self, agent_id: &str) -> Option<&WorkflowAgent> {
        self.agents
            .iter()
            .find(|agent| agent.agent_id.as_deref() == Some(agent_id))
    }
}

/// Replacement snapshot handed to the pane and the view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowSnapshot {
    pub session_id: String,
    pub runs: Vec<WorkflowRun>,
}

impl WorkflowSnapshot {
    /// Whether anything still needs refreshing; the pane's timer runs only
    /// while this holds.
    pub fn has_active_run(&self) -> bool {
        self.runs.iter().any(|run| !run.state.is_terminal())
    }
}

impl From<WorkflowRunState> for &'static str {
    fn from(value: WorkflowRunState) -> Self {
        match value {
            WorkflowRunState::Starting => "Starting",
            WorkflowRunState::Running => "Running",
            WorkflowRunState::Done => "Done",
            WorkflowRunState::Failed => "Failed",
            WorkflowRunState::Stopped => "Stopped",
        }
    }
}

impl From<WorkflowAgentState> for &'static str {
    fn from(value: WorkflowAgentState) -> Self {
        match value {
            WorkflowAgentState::Queued => "Queued",
            WorkflowAgentState::Running => "Running",
            WorkflowAgentState::Done => "Done",
            WorkflowAgentState::Failed => "Failed",
            WorkflowAgentState::Stopped => "Stopped",
        }
    }
}

/// Blocking reads owned by a provider and scheduled by the session executor.
pub trait WorkflowSource: Send + Sync {
    fn restore(&self, cwd: Option<&str>, session_id: &str) -> Result<Vec<WorkflowRun>, String>;

    fn refresh(
        &self,
        cwd: Option<&str>,
        session_id: &str,
        request: &WorkflowRefreshRequest,
    ) -> WorkflowRefreshResult;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkflowRefreshRequest {
    pub task_id: String,
    pub agent_ids: Vec<String>,
    pub open_agent: Option<String>,

    /// Last revision accepted by the conversation owner. Sources choose the
    /// revision; consumers only compare it or return it on the next read.
    pub transcript_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowTranscriptRead {
    pub agent_id: String,
    pub items: Vec<Item>,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowAgentProgress {
    pub agent_id: String,
    pub state: WorkflowAgentState,
    pub result: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkflowRefresh {
    pub run_id: Option<String>,
    pub agents: Vec<WorkflowAgentProgress>,
    pub failed: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorkflowRefreshResult {
    pub task_id: String,
    pub refresh: WorkflowRefresh,
    pub transcript: Option<WorkflowTranscriptRead>,
}
