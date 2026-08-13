//! Backend-neutral model for a Dynamic Workflow run: the phases it moves
//! through, the agents it fans out to, and their progress.
//!
//! Only Claude Code reports workflows today, so this module has one producer.
//! It still sits beside the provider adapters rather than inside one, because
//! the shared chat vocabulary carries these types and must not depend on a
//! provider module to do it.

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

    pub fn label(self) -> &'static str {
        match self {
            Self::Starting => "Starting",
            Self::Running => "Running",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
        }
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

impl WorkflowAgentState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Running",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
        }
    }
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
    /// Names the agent's persisted transcript file, so it is what links a row
    /// to its conversation.
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
    /// Directory identity, resolved from disk. A run has none until either its
    /// completion snapshot exists or one of its agents has been persisted.
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
    /// Set when the run's on-disk record could not be read. It reports a
    /// refresh problem and never means the run itself failed.
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
