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
use nmt_agent::chat::Item as SessionItem;
use nmt_agent::claude_code::workflows::{
    self, RestoredWorkflowRun, WorkflowRefreshRequest, WorkflowRefreshResult,
};
pub use nmt_agent::session::workflows::OpenWorkflowAgent;
use nmt_agent::session::workflows::WorkflowData;
use nmt_agent::workflow::{WorkflowRun, WorkflowSnapshot};

use crate::capabilities::AgentCapabilities as _;
use crate::session::Backend;
use crate::{AgentPane, AgentPaneEvent};
#[derive(Default)]
pub(crate) struct WorkflowUi {
    pub(crate) data: WorkflowData,
    visible: bool,
    refresh: Option<Task<()>>,
}

impl WorkflowUi {
    pub(crate) fn runs(&self) -> &[WorkflowRun] {
        self.data.runs()
    }

    pub(crate) fn running_agents(&self) -> usize {
        self.data.running_agents()
    }

    pub(crate) fn open_conversation(&self) -> Option<&OpenWorkflowAgent> {
        self.data.open_conversation()
    }

    fn clear(&mut self) {
        self.data.clear();
        self.refresh = None;
    }
    /// Show or hide the view, reporting whether that is a change.
    fn set_visible(&mut self, visible: bool) -> bool {
        let changed = self.visible != visible;

        self.visible = visible;

        changed
    }

    fn should_refresh(&self, reads_from_disk: bool) -> bool {
        reads_from_disk && self.visible && self.data.has_active_run()
    }
}

use nmt_agent::session::workflows::RefreshPlan;

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
        if self.workflows.data.set_snapshot(snapshot) {
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
        if self
            .workflows
            .data
            .apply_transcript(task_id, agent_id, items)
        {
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
            .backend()
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
        self.workflows.data.open_agent(task_id, agent_id);
        self.read_open_workflow_agent(cx);
        cx.notify();
    }

    pub fn close_workflow_agent(&mut self, cx: &mut Context<Self>) {
        self.workflows.data.close_agent();
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
            .backend()
            .and_then(Backend::session_id)
            .map(str::to_owned)
        else {
            return;
        };

        if !self.workflows.data.claim_restore(&session_id) {
            return;
        }

        let cwd = self.cwd();
        let epoch = self.runtime.epoch();
        let read = cx
            .background_executor()
            .spawn(async move { workflows::read_run_snapshots(cwd.as_deref(), &session_id) });

        cx.spawn(async move |this, cx| {
            let restored = read.await;

            this.update(cx, |this, cx| {
                // A restoration that outlived its session says nothing about
                // the conversation now open.
                if !this.runtime.is_current(epoch) {
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
            self.workflows.data.forget_restore();
            return;
        };

        let Some(session) = self.runtime.backend_mut() else {
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

        self.workflows.data.refresh_plan(&self.runtime, self.cwd())
    }

    /// Fold a tick's reads in. Returns whether the loop should keep running.
    fn apply_workflow_refresh_results(
        &mut self,
        epoch: u64,
        results: Vec<WorkflowRefreshResult>,
        cx: &mut Context<Self>,
    ) -> bool {
        // A tick that outlived its session must not touch the new one.
        if !self.runtime.is_current(epoch) {
            return false;
        }

        for result in results {
            self.workflows.data.note_open_len(&result);

            let Some(session) = self.runtime.backend_mut() else {
                return false;
            };

            for event in session.apply_workflow_refresh(result) {
                self.apply_event(event, cx);
            }
        }

        if self.workflows.data.mark_open_availability() {
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

            if let Some(session) = self.runtime.backend_mut() {
                session.request_workflow_agent_transcript(&task_id, &agent_id);
            }

            return;
        }

        let Some(session_id) = self
            .runtime
            .backend()
            .and_then(Backend::session_id)
            .map(str::to_owned)
        else {
            return;
        };

        let request = WorkflowRefreshRequest {
            task_id: open.task_id.clone(),
            agent_ids: self.workflows.data.agent_ids(&open.task_id),
            open_agent: Some(open.agent_id.clone()),
            open_agent_len: None,
        };

        let cwd = self.cwd();
        let epoch = self.runtime.epoch();
        let read = cx
            .background_executor()
            .spawn(async move { workflows::refresh_run(cwd.as_deref(), &session_id, &request) });

        cx.spawn(async move |this, cx| {
            let result = read.await;

            this.update(cx, |this, cx| {
                if !this.runtime.is_current(epoch) {
                    return;
                }

                this.apply_workflow_refresh_results(epoch, vec![result], cx);
            })
            .ok();
        })
        .detach();
    }
}
