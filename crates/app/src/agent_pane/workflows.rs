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

use nmt_agent_utils::claude_code::workflows::{
    self, RestoredWorkflowRun, WorkflowRefreshRequest, WorkflowRefreshResult,
};

use crate::agent_pane::*;

/// The agent conversation the user has open, and what has been read of it.
#[derive(Default)]
pub(crate) struct OpenWorkflowAgent {
    pub(crate) task_id: String,
    pub(crate) agent_id: String,
    pub(crate) items: Vec<SessionItem>,
    /// Size the transcript had when `items` was parsed, so an unchanged file
    /// is never re-parsed.
    len: Option<u64>,
    /// The provider has not persisted this agent's transcript; the row stays
    /// listed and the conversation reports itself unavailable.
    pub(crate) unavailable: bool,
    /// Bumped whenever `items` changes, so the transcript view rebuilds only
    /// on a real change.
    revision: u64,
}

impl OpenWorkflowAgent {
    pub(crate) fn revision(&self) -> u64 {
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
        let before = self.workflow_activity();
        self.workflows.snapshot = Some(snapshot);
        // The chrome reveals its control and shows a running count, so it is
        // told when either changes rather than on every refreshed snapshot.
        if self.workflow_activity() != before {
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
        let Some(open) = self.workflows.open.as_mut() else {
            return;
        };
        // The user may have moved on while the read was in flight.
        if open.task_id != task_id || open.agent_id != agent_id {
            return;
        }
        open.items = items;
        open.unavailable = false;
        open.revision += 1;
        cx.notify();
    }

    /// Show or hide the view. Refreshing follows visibility, so this is what
    /// starts and stops the one-second poll.
    pub(crate) fn set_workflows_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.workflows.visible == visible {
            return;
        }
        self.workflows.visible = visible;
        if visible {
            self.restore_workflows(cx);
        }
        self.sync_workflow_refresh(cx);
        cx.notify();
    }

    /// Session id when this pane runs Claude Code, the only provider that
    /// reports workflows.
    pub(crate) fn claude_session_id(&self) -> Option<String> {
        if self.kind != AgentKind::Claude {
            return None;
        }
        self.session
            .as_ref()
            .and_then(Backend::session_id)
            .map(str::to_owned)
    }

    /// Runs of the scoped session, in provider order.
    pub(crate) fn workflow_runs(&self) -> &[WorkflowRun] {
        self.workflows.runs()
    }

    /// Agents of this tab the provider currently reports as running.
    pub(crate) fn running_workflow_agents(&self) -> usize {
        self.workflows
            .runs()
            .iter()
            .flat_map(|run| run.agents.iter())
            .filter(|agent| agent.state == WorkflowAgentState::Running)
            .count()
    }

    /// What the chrome derives from this tab: whether a control is warranted
    /// at all, and the number it shows.
    fn workflow_activity(&self) -> (bool, usize) {
        (
            !self.workflows.runs().is_empty(),
            self.running_workflow_agents(),
        )
    }

    /// The agent conversation the user has open, if any.
    pub(crate) fn open_workflow_conversation(&self) -> Option<&OpenWorkflowAgent> {
        self.workflows.open.as_ref()
    }

    /// Open one agent's conversation, reading it immediately rather than
    /// waiting for the next tick.
    pub(crate) fn open_workflow_agent(
        &mut self,
        task_id: &str,
        agent_id: &str,
        cx: &mut Context<Self>,
    ) {
        self.workflows.open = Some(OpenWorkflowAgent {
            task_id: task_id.to_owned(),
            agent_id: agent_id.to_owned(),
            ..OpenWorkflowAgent::default()
        });
        self.read_open_workflow_agent(cx);
        cx.notify();
    }

    pub(crate) fn close_workflow_agent(&mut self, cx: &mut Context<Self>) {
        self.workflows.open = None;
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
        let Some(session_id) = self
            .session
            .as_ref()
            .and_then(Backend::session_id)
            .map(str::to_owned)
        else {
            return;
        };
        if self.workflows.restored_session.as_deref() == Some(session_id.as_str()) {
            return;
        }
        self.workflows.restored_session = Some(session_id.clone());

        let cwd = self.cwd.clone();
        let epoch = self.session_epoch;
        let read = cx
            .background_executor()
            .spawn(async move { workflows::read_run_snapshots(cwd.as_deref(), &session_id) });

        cx.spawn(async move |this, cx| {
            let restored = read.await;
            this.update(cx, |this, cx| {
                // A restoration that outlived its session says nothing about
                // the conversation now open.
                if !is_current_session_epoch(this.session_epoch, epoch) {
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
            self.workflows.restored_session = None;
            return;
        };
        let Some(session) = self.session.as_mut() else {
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
        self.workflows.visible
            && self
                .workflows
                .snapshot
                .as_ref()
                .is_some_and(WorkflowSnapshot::has_active_run)
    }

    fn workflow_refresh_plan(&self) -> Option<RefreshPlan> {
        if !self.should_refresh_workflows() {
            return None;
        }
        let session = self.session.as_ref()?;
        let session_id = session.session_id()?.to_owned();

        let open = self.workflows.open.as_ref();
        let requests = session
            .workflow_refresh_requests()
            .into_iter()
            .map(|mut request| {
                // The open conversation is read as part of its own run's tick,
                // so a tick still touches at most one transcript.
                if let Some(open) = open.filter(|open| open.task_id == request.task_id) {
                    request.open_agent = Some(open.agent_id.clone());
                    request.open_agent_len = open.len;
                }
                request
            })
            .collect::<Vec<_>>();

        (!requests.is_empty()).then_some(RefreshPlan {
            cwd: self.cwd.clone(),
            session_id,
            epoch: self.session_epoch,
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
        if !is_current_session_epoch(self.session_epoch, epoch) {
            return false;
        }

        for result in results {
            if let Some(transcript) = result.transcript.as_ref()
                && let Some(open) = self.workflows.open.as_mut()
                && open.task_id == result.task_id
                && open.agent_id == transcript.agent_id
            {
                open.len = Some(transcript.len);
            }
            let Some(session) = self.session.as_mut() else {
                return false;
            };
            for event in session.apply_workflow_refresh(result) {
                self.apply_event(event, cx);
            }
        }

        self.mark_open_workflow_agent_availability(cx);
        self.should_refresh_workflows()
    }

    /// Read the open conversation once, outside the tick cadence.
    fn read_open_workflow_agent(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.workflows.open.as_ref() else {
            return;
        };
        let Some(session_id) = self
            .session
            .as_ref()
            .and_then(Backend::session_id)
            .map(str::to_owned)
        else {
            return;
        };

        let request = WorkflowRefreshRequest {
            task_id: open.task_id.clone(),
            agent_ids: self.workflow_agent_ids(&open.task_id),
            open_agent: Some(open.agent_id.clone()),
            open_agent_len: None,
        };
        let cwd = self.cwd.clone();
        let epoch = self.session_epoch;
        let read = cx
            .background_executor()
            .spawn(async move { workflows::refresh_run(cwd.as_deref(), &session_id, &request) });

        cx.spawn(async move |this, cx| {
            let result = read.await;
            this.update(cx, |this, cx| {
                if !is_current_session_epoch(this.session_epoch, epoch) {
                    return;
                }
                this.apply_workflow_refresh_results(epoch, vec![result], cx);
            })
            .ok();
        })
        .detach();
    }

    fn workflow_agent_ids(&self, task_id: &str) -> Vec<String> {
        self.workflows
            .runs()
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
    fn mark_open_workflow_agent_availability(&mut self, cx: &mut Context<Self>) {
        let settled = self
            .workflows
            .open
            .as_ref()
            .map(|open| open.task_id.clone())
            .and_then(|task_id| {
                self.workflows
                    .runs()
                    .iter()
                    .find(|run| run.task_id == task_id)
                    .map(|run| run.state.is_terminal())
            })
            .unwrap_or(false);

        let Some(open) = self.workflows.open.as_mut() else {
            return;
        };
        let unavailable = settled && open.items.is_empty();
        if open.unavailable != unavailable {
            open.unavailable = unavailable;
            cx.notify();
        }
    }
}
