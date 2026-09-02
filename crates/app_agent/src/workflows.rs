//! Workflow-run state for the `Workflows` view, and the refresh that keeps it
//! current.
//!
//! Run and agent state arrive on the provider stream, but an agent's own
//! conversation is never streamed — it exists only as a file the provider
//! appends to. A run that is still going is therefore polled once a second, so
//! an agent finishing during a quiet stretch of the stream shows up promptly
//! and an open conversation extends while its agent is still writing.
//!
//! Each tick reads one small journal per active run plus, at most, the one
//! conversation the user has open. That bound is the reason the poll is cheap
//! enough to run every second regardless of how many agents a run fans out to.

use std::time::Duration;

use gpui::{Context, Task};
use nmt_agent_utils::chat::Item as SessionItem;
use nmt_agent_utils::claude_code::workflows::{
    self, RestoredWorkflowRun, WorkflowRefreshRequest, WorkflowRefreshResult,
};
use nmt_agent_utils::workflow::{WorkflowAgentState, WorkflowRun, WorkflowSnapshot};

use crate::commands::is_current_session_epoch;
use crate::session::Backend;
use crate::{AgentPane, AgentPaneEvent};

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
pub(crate) struct WorkflowUi {
    pub(crate) snapshot: Option<WorkflowSnapshot>,
    pub(crate) open: Option<OpenWorkflowAgent>,
    /// Whether the right-side area currently shows this view. Polling a file
    /// for a view nobody is looking at is pure cost, so it gates the refresh.
    visible: bool,
    /// Session whose completed runs were already read back from disk, so a
    /// resumed conversation restores once rather than on every reopen.
    restored_session: Option<String>,
    refresh: Option<Task<()>>,
}

impl WorkflowUi {
    /// Runs of the scoped session, empty until one is reported.
    pub(crate) fn runs(&self) -> &[WorkflowRun] {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.runs.as_slice())
            .unwrap_or_default()
    }

    fn clear(&mut self) {
        self.snapshot = None;
        self.open = None;
        self.restored_session = None;
        self.refresh = None;
    }

    /// Agents of this tab the provider currently reports as running.
    pub(crate) fn running_agents(&self) -> usize {
        self.runs()
            .iter()
            .flat_map(|run| run.agents.iter())
            .filter(|agent| agent.state == WorkflowAgentState::Running)
            .count()
    }

    /// What the chrome derives from this tab: whether a control is warranted
    /// at all, and the number it shows.
    fn activity(&self) -> (bool, usize) {
        (!self.runs().is_empty(), self.running_agents())
    }

    /// Take a replacement snapshot, reporting whether what the chrome shows
    /// changed. The chrome reveals its control and shows a running count, so
    /// it is told on a change rather than on every refreshed snapshot.
    fn set_snapshot(&mut self, snapshot: WorkflowSnapshot) -> bool {
        let before = self.activity();

        self.snapshot = Some(snapshot);

        self.activity() != before
    }

    /// The agent conversation the user has open, if any.
    pub(crate) fn open_conversation(&self) -> Option<&OpenWorkflowAgent> {
        self.open.as_ref()
    }

    fn open_agent(&mut self, task_id: &str, agent_id: &str) {
        self.open = Some(OpenWorkflowAgent {
            task_id: task_id.to_owned(),
            agent_id: agent_id.to_owned(),
            ..OpenWorkflowAgent::default()
        });
    }

    fn close_agent(&mut self) {
        self.open = None;
    }

    /// Show or hide the view, reporting whether that is a change.
    fn set_visible(&mut self, visible: bool) -> bool {
        let changed = self.visible != visible;

        self.visible = visible;

        changed
    }

    /// Fold one agent conversation in, reporting whether it is still the one
    /// on screen. The user may have moved on while the read was in flight.
    fn apply_transcript(&mut self, task_id: &str, agent_id: &str, items: Vec<SessionItem>) -> bool {
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

    fn agent_ids(&self, task_id: &str) -> Vec<String> {
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
    fn mark_open_availability(&mut self) -> bool {
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

    /// Whether the poll has anything to find. A run that reports itself as it
    /// goes leaves nothing for a poll to read: every tick would re-read what
    /// the events already delivered.
    fn should_refresh(&self, reads_from_disk: bool) -> bool {
        reads_from_disk
            && self.visible
            && self
                .snapshot
                .as_ref()
                .is_some_and(WorkflowSnapshot::has_active_run)
    }

    /// Attach the open conversation to the request for the run it belongs to,
    /// so a tick still touches at most one transcript.
    fn scope_requests(&self, requests: Vec<WorkflowRefreshRequest>) -> Vec<WorkflowRefreshRequest> {
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
    fn note_open_len(&mut self, result: &WorkflowRefreshResult) {
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
    fn claim_restore(&mut self, session_id: &str) -> bool {
        if self.restored_session.as_deref() == Some(session_id) {
            return false;
        }

        self.restored_session = Some(session_id.to_owned());

        true
    }

    /// Give the claim back after a failed read, so the next open retries.
    fn forget_restore(&mut self) {
        self.restored_session = None;
    }
}

/// What one refresh tick should read, captured before its IO starts.
struct RefreshPlan {
    cwd: Option<String>,
    session_id: String,
    epoch: u64,
    requests: Vec<WorkflowRefreshRequest>,
}

impl AgentPane {
    /// Drop every run when the pane moves to another conversation.
    pub(super) fn clear_workflows(&mut self) {
        self.workflows.clear();
    }

    /// Replacement snapshot from the provider stream.
    pub(super) fn apply_workflow_snapshot(
        &mut self,
        snapshot: WorkflowSnapshot,
        cx: &mut Context<Self>,
    ) {
        if self.workflows.set_snapshot(snapshot) {
            cx.emit(AgentPaneEvent::WorkflowActivity);
        }
        // A run that just started is what makes refreshing worth doing.
        self.sync_workflow_refresh(cx);
        cx.notify();
    }

    /// One agent conversation read from disk.
    pub(super) fn apply_workflow_transcript(
        &mut self,
        task_id: &str,
        agent_id: &str,
        items: Vec<SessionItem>,
        cx: &mut Context<Self>,
    ) {
        if self.workflows.apply_transcript(task_id, agent_id, items) {
            cx.notify();
        }
    }

    /// Show or hide the view. Refreshing follows visibility, so this is what
    /// starts and stops the one-second poll.
    pub fn set_workflows_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if !self.workflows.set_visible(visible) {
            return;
        }
        if visible {
            self.restore_workflows(cx);
        }
        self.sync_workflow_refresh(cx);
        cx.notify();
    }

    /// Session id when this pane runs a harness that reports workflows, which
    /// is what scopes runs to the conversation they belong to.
    pub fn workflow_session_id(&self) -> Option<String> {
        if !self.kind.caps().workflows {
            return None;
        }
        self.runtime
            .backend
            .as_ref()
            .and_then(Backend::session_id)
            .map(str::to_owned)
    }

    /// Runs of the scoped session, in provider order.
    pub fn workflow_runs(&self) -> &[WorkflowRun] {
        self.workflows.runs()
    }

    /// Agents of this tab the provider currently reports as running.
    pub fn running_workflow_agents(&self) -> usize {
        self.workflows.running_agents()
    }

    /// The agent conversation the user has open, if any.
    pub fn open_workflow_conversation(&self) -> Option<&OpenWorkflowAgent> {
        self.workflows.open_conversation()
    }

    /// Open one agent's conversation, reading it immediately rather than
    /// waiting for the next tick.
    pub fn open_workflow_agent(&mut self, task_id: &str, agent_id: &str, cx: &mut Context<Self>) {
        self.workflows.open_agent(task_id, agent_id);
        self.read_open_workflow_agent(cx);
        cx.notify();
    }

    pub fn close_workflow_agent(&mut self, cx: &mut Context<Self>) {
        self.workflows.close_agent();
        cx.notify();
    }

    /// Read every completed run this session already recorded. A resumed
    /// conversation replays nothing, so its finished runs exist only on disk.
    ///
    /// Runs once per session, from whichever comes first: the session becoming
    /// ready, or the view opening. The ready path is what lets a resumed
    /// conversation surface the title-bar control at all — the view cannot be
    /// opened before the control exists, so waiting for it would strand every
    /// run recorded before this tab opened.
    pub(super) fn restore_workflows(&mut self, cx: &mut Context<Self>) {
        // A harness that reports its runs live replays them with the rest of
        // the conversation, so there is no stored record to go looking for.
        if !self.kind.caps().workflows_read_from_disk {
            return;
        }

        let Some(session_id) = self
            .runtime
            .backend
            .as_ref()
            .and_then(Backend::session_id)
            .map(str::to_owned)
        else {
            return;
        };
        if !self.workflows.claim_restore(&session_id) {
            return;
        }

        let cwd = self.cwd();
        let epoch = self.runtime.epoch;
        let read = cx
            .background_executor()
            .spawn(async move { workflows::read_run_snapshots(cwd.as_deref(), &session_id) });

        cx.spawn(async move |this, cx| {
            let restored = read.await;
            this.update(cx, |this, cx| {
                // A restoration that outlived its session says nothing about
                // the conversation now open.
                if !is_current_session_epoch(this.runtime.epoch, epoch) {
                    return;
                }
                this.merge_restored_workflows(restored, cx);
            })
            .ok();
        })
        .detach();
    }

    fn merge_restored_workflows(
        &mut self,
        restored: Result<Vec<RestoredWorkflowRun>, String>,
        cx: &mut Context<Self>,
    ) {
        // A failed read leaves whatever the live stream reported; the view is
        // still usable and the next open retries.
        let Ok(restored) = restored else {
            self.workflows.forget_restore();
            return;
        };
        let Some(session) = self.runtime.backend.as_mut() else {
            return;
        };
        for event in session.restore_workflows(restored) {
            self.apply_event(event, cx);
        }
    }

    /// Start the poll when there is something to poll, stop it otherwise.
    fn sync_workflow_refresh(&mut self, cx: &mut Context<Self>) {
        if !self.should_refresh_workflows() {
            self.workflows.refresh = None;
            return;
        }
        if self.workflows.refresh.is_some() {
            return;
        }

        self.workflows.refresh = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;

                let Ok(Some(plan)) = this.update(cx, |this, _| this.workflow_refresh_plan()) else {
                    break;
                };
                let cwd = plan.cwd;
                let session_id = plan.session_id;
                let requests = plan.requests;
                // A tick does its own reads before the next beat, so ticks can
                // fall behind but never overlap or queue up.
                let results = cx
                    .background_executor()
                    .spawn(async move {
                        requests
                            .iter()
                            .map(|request| {
                                workflows::refresh_run(cwd.as_deref(), &session_id, request)
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;

                let applied = this.update(cx, |this, cx| {
                    this.apply_workflow_refresh_results(plan.epoch, results, cx)
                });
                if !matches!(applied, Ok(true)) {
                    break;
                }
            }
        }));
    }

    fn should_refresh_workflows(&self) -> bool {
        self.workflows
            .should_refresh(self.kind.caps().workflows_read_from_disk)
    }

    fn workflow_refresh_plan(&self) -> Option<RefreshPlan> {
        if !self.should_refresh_workflows() {
            return None;
        }
        let session = self.runtime.backend.as_ref()?;
        let session_id = session.session_id()?.to_owned();

        let requests = self
            .workflows
            .scope_requests(session.workflow_refresh_requests());

        (!requests.is_empty()).then_some(RefreshPlan {
            cwd: self.cwd(),
            session_id,
            epoch: self.runtime.epoch,
            requests,
        })
    }

    /// Fold a tick's reads in. Returns whether the loop should keep running.
    fn apply_workflow_refresh_results(
        &mut self,
        epoch: u64,
        results: Vec<WorkflowRefreshResult>,
        cx: &mut Context<Self>,
    ) -> bool {
        // A tick that outlived its session must not touch the new one.
        if !is_current_session_epoch(self.runtime.epoch, epoch) {
            return false;
        }

        for result in results {
            self.workflows.note_open_len(&result);

            let Some(session) = self.runtime.backend.as_mut() else {
                return false;
            };
            for event in session.apply_workflow_refresh(result) {
                self.apply_event(event, cx);
            }
        }

        if self.workflows.mark_open_availability() {
            cx.notify();
        }

        self.should_refresh_workflows()
    }

    /// Read the open conversation once, outside the tick cadence.
    fn read_open_workflow_agent(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.workflows.open_conversation() else {
            return;
        };

        // A harness that reports its runs live has no stored record to read:
        // the member is a conversation of its own on the host, and asking for
        // it is one request whose answer arrives as an ordinary event.
        if !self.kind.caps().workflows_read_from_disk {
            let (task_id, agent_id) = (open.task_id.clone(), open.agent_id.clone());
            if let Some(session) = self.runtime.backend.as_mut() {
                session.request_workflow_agent_transcript(&task_id, &agent_id);
            }
            return;
        }

        let Some(session_id) = self
            .runtime
            .backend
            .as_ref()
            .and_then(Backend::session_id)
            .map(str::to_owned)
        else {
            return;
        };

        let request = WorkflowRefreshRequest {
            task_id: open.task_id.clone(),
            agent_ids: self.workflows.agent_ids(&open.task_id),
            open_agent: Some(open.agent_id.clone()),
            open_agent_len: None,
        };
        let cwd = self.cwd();
        let epoch = self.runtime.epoch;
        let read = cx
            .background_executor()
            .spawn(async move { workflows::refresh_run(cwd.as_deref(), &session_id, &request) });

        cx.spawn(async move |this, cx| {
            let result = read.await;
            this.update(cx, |this, cx| {
                if !is_current_session_epoch(this.runtime.epoch, epoch) {
                    return;
                }
                this.apply_workflow_refresh_results(epoch, vec![result], cx);
            })
            .ok();
        })
        .detach();
    }
}
