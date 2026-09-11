use crate::chat::Item as SessionItem;
use crate::claude_code::workflows::{WorkflowRefreshRequest, WorkflowRefreshResult};
use crate::session::lifecycle::SessionRuntime;
use crate::workflow::{WorkflowAgentState, WorkflowRun, WorkflowSnapshot};

/// The agent conversation the user has open, and what has been read of it.
#[derive(Default)]
pub struct OpenWorkflowAgent {
    pub task_id: String,
    pub agent_id: String,
    pub items: Vec<SessionItem>,

    /// Size the transcript had when `items` was parsed, so an unchanged file
    /// is never re-parsed.
    len: Option<u64>,

    /// The provider has not persisted this agent's transcript; the row stays
    /// listed and the conversation reports itself unavailable.
    pub unavailable: bool,

    /// Bumped whenever `items` changes, so the transcript view rebuilds only
    /// on a real change.
    revision: u64,
}

impl OpenWorkflowAgent {
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

/// Workflow state the pane owns and the view renders.
#[derive(Default)]
pub struct WorkflowData {
    pub snapshot: Option<WorkflowSnapshot>,
    pub open: Option<OpenWorkflowAgent>,

    /// Session whose completed runs were already read back from disk, so a
    /// resumed conversation restores once rather than on every reopen.
    restored_session: Option<String>,
}

impl WorkflowData {
    /// Runs of the scoped session, empty until one is reported.
    pub fn runs(&self) -> &[WorkflowRun] {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.runs.as_slice())
            .unwrap_or_default()
    }

    pub fn clear(&mut self) {
        self.snapshot = None;
        self.open = None;
        self.restored_session = None;
    }

    /// Agents of this tab the provider currently reports as running.
    pub fn running_agents(&self) -> usize {
        self.runs()
            .iter()
            .flat_map(|run| run.agents.iter())
            .filter(|agent| agent.state == WorkflowAgentState::Running)
            .count()
    }

    /// What the chrome derives from this tab: whether a control is warranted
    /// at all, and the number it shows.
    pub fn activity(&self) -> (bool, usize) {
        (!self.runs().is_empty(), self.running_agents())
    }

    /// Take a replacement snapshot, reporting whether what the chrome shows
    /// changed. The chrome reveals its control and shows a running count, so
    /// it is told on a change rather than on every refreshed snapshot.
    pub fn set_snapshot(&mut self, snapshot: WorkflowSnapshot) -> bool {
        let before = self.activity();

        self.snapshot = Some(snapshot);

        self.activity() != before
    }

    /// The agent conversation the user has open, if any.
    pub fn open_conversation(&self) -> Option<&OpenWorkflowAgent> {
        self.open.as_ref()
    }

    pub fn open_agent(&mut self, task_id: &str, agent_id: &str) {
        self.open = Some(OpenWorkflowAgent {
            task_id: task_id.to_owned(),
            agent_id: agent_id.to_owned(),
            ..OpenWorkflowAgent::default()
        });
    }

    pub fn close_agent(&mut self) {
        self.open = None;
    }

    /// Fold one agent conversation in, reporting whether it is still the one
    /// on screen. The user may have moved on while the read was in flight.
    pub fn apply_transcript(
        &mut self,
        task_id: &str,
        agent_id: &str,
        items: Vec<SessionItem>,
    ) -> bool {
        let Some(open) = self.open.as_mut() else {
            return false;
        };

        if open.task_id != task_id || open.agent_id != agent_id {
            return false;
        }

        open.items = items;
        open.unavailable = false;
        open.revision += 1;

        true
    }

    pub fn agent_ids(&self, task_id: &str) -> Vec<String> {
        self.runs()
            .iter()
            .find(|run| run.task_id == task_id)
            .map(|run| {
                run.agents
                    .iter()
                    .filter_map(|agent| agent.agent_id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// An open conversation with nothing read yet reports itself unavailable
    /// once its run has settled, because no further content is coming.
    /// Reports whether that answer changed.
    pub fn mark_open_availability(&mut self) -> bool {
        let settled = self
            .open
            .as_ref()
            .map(|open| open.task_id.clone())
            .and_then(|task_id| {
                self.runs()
                    .iter()
                    .find(|run| run.task_id == task_id)
                    .map(|run| run.state.is_terminal())
            })
            .unwrap_or(false);

        let Some(open) = self.open.as_mut() else {
            return false;
        };

        let unavailable = settled && open.items.is_empty();

        if open.unavailable == unavailable {
            return false;
        }

        open.unavailable = unavailable;

        true
    }

    pub fn has_active_run(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(WorkflowSnapshot::has_active_run)
    }

    /// Attach the open conversation to the request for the run it belongs to,
    /// so a tick still touches at most one transcript.
    pub fn scope_requests(
        &self,
        requests: Vec<WorkflowRefreshRequest>,
    ) -> Vec<WorkflowRefreshRequest> {
        let open = self.open.as_ref();

        requests
            .into_iter()
            .map(|mut request| {
                if let Some(open) = open.filter(|open| open.task_id == request.task_id) {
                    request.open_agent = Some(open.agent_id.clone());
                    request.open_agent_len = open.len;
                }

                request
            })
            .collect()
    }

    /// Record how much of the open transcript a tick read, so an unchanged
    /// file is never re-parsed.
    pub fn note_open_len(&mut self, result: &WorkflowRefreshResult) {
        let Some(transcript) = result.transcript.as_ref() else {
            return;
        };

        let Some(open) = self.open.as_mut() else {
            return;
        };

        if open.task_id == result.task_id && open.agent_id == transcript.agent_id {
            open.len = Some(transcript.len);
        }
    }

    /// Claim the one restore this session gets, so a resumed conversation
    /// reads its stored runs once rather than on every reopen.
    pub fn claim_restore(&mut self, session_id: &str) -> bool {
        if self.restored_session.as_deref() == Some(session_id) {
            return false;
        }

        self.restored_session = Some(session_id.to_owned());

        true
    }

    /// Give the claim back after a failed read, so the next open retries.
    pub fn forget_restore(&mut self) {
        self.restored_session = None;
    }
}

pub struct RefreshPlan {
    pub cwd: Option<String>,
    pub session_id: String,
    pub epoch: u64,
    pub requests: Vec<WorkflowRefreshRequest>,
}

impl WorkflowData {
    pub fn refresh_plan(
        &self,
        runtime: &SessionRuntime,
        cwd: Option<String>,
    ) -> Option<RefreshPlan> {
        let session = runtime.backend()?;
        let session_id = session.session_id()?.to_owned();
        let requests = self.scope_requests(session.workflow_refresh_requests());

        (!requests.is_empty()).then_some(RefreshPlan {
            cwd,
            session_id,
            epoch: runtime.epoch(),
            requests,
        })
    }
}
