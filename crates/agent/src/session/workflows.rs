use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::chat::Item as SessionItem;
use crate::session::lifecycle::SessionRuntime;
use crate::transcript::conversation::ConversationState;
use crate::workflow::{
    WorkflowAgentState, WorkflowRefreshRequest, WorkflowRefreshResult, WorkflowRun,
    WorkflowSnapshot, WorkflowSource,
};

/// The agent conversation the user has open, and what has been read of it.
#[derive(Default)]
pub struct OpenWorkflowAgent {
    pub task_id: String,
    pub agent_id: String,
    pub conversation: Rc<RefCell<ConversationState>>,
    readers: Rc<Cell<usize>>,

    /// Last source revision accepted for this conversation.
    source_revision: Option<u64>,

    /// The provider has not persisted this agent's transcript; the row stays
    /// listed and the conversation reports itself unavailable.
    pub unavailable: bool,
}

/// Workflow state the pane owns and the view renders.
#[derive(Default)]
pub struct WorkflowData {
    pub snapshot: Option<WorkflowSnapshot>,
    conversations: HashMap<(String, String), OpenWorkflowAgent>,

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

        for open in self.conversations.values() {
            open.conversation.borrow_mut().clear();
        }

        self.conversations.clear();
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
    pub(crate) fn set_snapshot(&mut self, snapshot: WorkflowSnapshot) -> bool {
        let before = self.activity();

        self.snapshot = Some(snapshot);

        self.activity() != before
    }

    pub fn conversation(&self, task_id: &str, agent_id: &str) -> Option<&OpenWorkflowAgent> {
        self.conversations
            .get(&(task_id.to_owned(), agent_id.to_owned()))
    }

    pub fn open_agent(&mut self, task_id: &str, agent_id: &str) -> WorkflowReader {
        let key = (task_id.to_owned(), agent_id.to_owned());

        let open = self
            .conversations
            .entry(key.clone())
            .or_insert_with(|| OpenWorkflowAgent {
                task_id: key.0.clone(),
                agent_id: key.1.clone(),
                ..Default::default()
            });

        open.readers.set(open.readers.get() + 1);

        WorkflowReader {
            key,
            readers: open.readers.clone(),
        }
    }

    /// Fold one agent conversation in, reporting whether it is still the one
    /// on screen. The user may have moved on while the read was in flight.
    pub(crate) fn apply_transcript(
        &mut self,
        task_id: &str,
        agent_id: &str,
        items: Vec<SessionItem>,
    ) -> bool {
        let Some(open) = self
            .conversations
            .get_mut(&(task_id.to_owned(), agent_id.to_owned()))
        else {
            return false;
        };

        let mut conversation = open.conversation.borrow_mut();

        conversation.clear();

        for item in items {
            conversation.push(0, item, Vec::new());
        }

        open.unavailable = false;

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
        let runs = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.runs.as_slice())
            .unwrap_or_default();

        let mut changed = false;

        for open in self.conversations.values_mut() {
            let settled = runs
                .iter()
                .find(|run| run.task_id == open.task_id)
                .is_some_and(|run| run.state.is_terminal());

            let unavailable = settled && open.conversation.borrow().content.entries().is_empty();

            changed |= open.unavailable != unavailable;
            open.unavailable = unavailable;
        }

        changed
    }

    pub fn has_active_run(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(WorkflowSnapshot::has_active_run)
    }

    /// Attach the open conversation to the request for the run it belongs to,
    /// so a tick still touches at most one transcript.
    pub(crate) fn scope_requests(
        &self,
        requests: Vec<WorkflowRefreshRequest>,
    ) -> Vec<WorkflowRefreshRequest> {
        let mut scoped = Vec::new();

        for request in requests {
            let mut readers = self
                .conversations
                .values()
                .filter(|open| open.task_id == request.task_id && open.readers.get() > 0)
                .peekable();

            if readers.peek().is_none() {
                scoped.push(request);

                continue;
            }

            for open in readers {
                scoped.push(WorkflowRefreshRequest {
                    task_id: request.task_id.clone(),
                    agent_ids: request.agent_ids.clone(),
                    open_agent: Some(open.agent_id.clone()),
                    transcript_revision: open.source_revision,
                });
            }
        }

        scoped
    }

    /// Acknowledge only results accepted for the current session and reader.
    pub fn accept_revision(&mut self, result: &WorkflowRefreshResult) {
        let Some(transcript) = result.transcript.as_ref() else {
            return;
        };

        let Some(open) = self
            .conversations
            .get_mut(&(result.task_id.clone(), transcript.agent_id.clone()))
        else {
            return;
        };

        if open.task_id == result.task_id && open.agent_id == transcript.agent_id {
            open.source_revision = Some(transcript.revision);
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

    pub fn refresh_plan(
        &self,
        runtime: &SessionRuntime,
        cwd: Option<String>,
    ) -> Option<RefreshPlan> {
        let session = runtime.backend()?;
        let session_id = session.session_id()?.to_owned();
        let source = session.workflow_source()?;
        let requests = self.scope_requests(session.workflow_refresh_requests());

        (!requests.is_empty()).then_some(RefreshPlan {
            cwd,
            session_id,
            epoch: runtime.epoch(),
            requests,
            source,
        })
    }
}

pub struct RefreshPlan {
    cwd: Option<String>,
    session_id: String,
    pub epoch: u64,
    requests: Vec<WorkflowRefreshRequest>,
    source: Arc<dyn WorkflowSource>,
}

impl RefreshPlan {
    pub fn read(self) -> Vec<WorkflowRefreshResult> {
        self.requests
            .iter()
            .map(|request| {
                self.source
                    .refresh(self.cwd.as_deref(), &self.session_id, request)
            })
            .collect()
    }
}

/// A reader keeps refresh interest in one member without owning execution.
pub struct WorkflowReader {
    pub key: (String, String),
    readers: Rc<Cell<usize>>,
}

impl Drop for WorkflowReader {
    fn drop(&mut self) {
        self.readers.set(self.readers.get().saturating_sub(1));
    }
}
