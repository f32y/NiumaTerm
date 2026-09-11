use std::time::Duration;

use gpui::Context;
use nmt_agent::claude_code::workflows::{
    self, RestoredWorkflowRun, WorkflowRefreshRequest, WorkflowRefreshResult,
};
use nmt_agent::session::Backend;
use nmt_agent::session::workflows::RefreshPlan;

use crate::capabilities::AgentCapabilities as _;
use crate::execution::AgentSession;

impl AgentSession {
    /// Read every completed run this session already recorded. A resumed
    /// conversation replays nothing, so its finished runs exist only on disk.
    ///
    /// Runs once per session, from whichever comes first: the session becoming
    /// ready, or the view opening. The ready path is what lets a resumed
    /// conversation surface the title-bar control at all — the view cannot be
    /// opened before the control exists, so waiting for it would strand every
    /// run recorded before this tab opened.
    pub(crate) fn restore_workflows(&mut self, cx: &mut Context<Self>) {
        // A harness that reports its runs live replays them with the rest of
        // the conversation, so there is no stored record to go looking for.
        if !self.kind.caps().workflows_read_from_disk {
            return;
        }

        let Some(session_id) = self
            .controller
            .borrow()
            .runtime
            .backend()
            .and_then(Backend::session_id)
            .map(str::to_owned)
        else {
            return;
        };

        if !self
            .controller
            .borrow_mut()
            .workflows
            .claim_restore(&session_id)
        {
            return;
        }

        let cwd = self.active_workspace.primary().map(str::to_owned);
        let epoch = self.controller.borrow().runtime.epoch();

        let read = cx
            .background_executor()
            .spawn(async move { workflows::read_run_snapshots(cwd.as_deref(), &session_id) });

        cx.spawn(async move |this, cx| {
            let restored = read.await;

            this.update(cx, |this, cx| {
                // A restoration that outlived its session says nothing about
                // the conversation now open.
                if this.is_closed() || !this.controller.borrow().runtime.is_current(epoch) {
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
            self.controller.borrow_mut().workflows.forget_restore();

            return;
        };

        let events = {
            let mut state = self.controller.borrow_mut();

            let Some(session) = state.runtime.backend_mut() else {
                return;
            };

            session.restore_workflows(restored)
        };

        for event in events {
            let epoch = self.controller.borrow().runtime.epoch();

            self.apply_event(epoch, event, cx);
        }
    }

    /// Start the poll when there is something to poll, stop it otherwise.
    pub(crate) fn sync_workflow_refresh(&mut self, cx: &mut Context<Self>) {
        if !self.should_refresh_workflows() {
            self.workflow_refresh = None;

            return;
        }

        if self.workflow_refresh.is_some() {
            return;
        }

        self.workflow_refresh = Some(cx.spawn(async move |this, cx| {
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

            let _ = this.update(cx, |this, _| this.workflow_refresh = None);
        }));
    }

    fn should_refresh_workflows(&self) -> bool {
        !self.is_closed()
            && self.workflow_readers.get() > 0
            && self.kind.caps().workflows_read_from_disk
            && self.controller.borrow().workflows.has_active_run()
    }

    fn workflow_refresh_plan(&self) -> Option<RefreshPlan> {
        if !self.should_refresh_workflows() {
            return None;
        }

        self.controller.borrow().workflows.refresh_plan(
            &self.controller.borrow().runtime,
            self.active_workspace.primary().map(str::to_owned),
        )
    }

    /// Fold a tick's reads in. Returns whether the loop should keep running.
    fn apply_workflow_refresh_results(
        &mut self,
        epoch: u64,
        results: Vec<WorkflowRefreshResult>,
        cx: &mut Context<Self>,
    ) -> bool {
        // A tick that outlived its session must not touch the new one.
        if self.is_closed() || !self.controller.borrow().runtime.is_current(epoch) {
            return false;
        }

        for result in results {
            self.controller
                .borrow_mut()
                .workflows
                .note_open_len(&result);

            let events = {
                let mut state = self.controller.borrow_mut();

                let Some(session) = state.runtime.backend_mut() else {
                    return false;
                };

                session.apply_workflow_refresh(result)
            };

            for event in events {
                let epoch = self.controller.borrow().runtime.epoch();

                self.apply_event(epoch, event, cx);
            }
        }

        if self
            .controller
            .borrow_mut()
            .workflows
            .mark_open_availability()
        {
            cx.notify();
        }

        self.should_refresh_workflows()
    }

    /// Read the open conversation once, outside the tick cadence.
    pub(crate) fn read_open_workflow_agent(
        &mut self,
        task_id: &str,
        agent_id: &str,
        cx: &mut Context<Self>,
    ) {
        let task_id = task_id.to_owned();
        let agent_id = agent_id.to_owned();

        // A harness that reports its runs live has no stored record to read:
        // the member is a conversation of its own on the host, and asking for
        // it is one request whose answer arrives as an ordinary event.
        if !self.kind.caps().workflows_read_from_disk {
            if let Some(session) = self.controller.borrow_mut().runtime.backend_mut() {
                session.request_workflow_agent_transcript(&task_id, &agent_id);
            }

            return;
        }

        let Some(session_id) = self
            .controller
            .borrow()
            .runtime
            .backend()
            .and_then(Backend::session_id)
            .map(str::to_owned)
        else {
            return;
        };

        let request = WorkflowRefreshRequest {
            task_id: task_id.clone(),
            agent_ids: self.controller.borrow().workflows.agent_ids(&task_id),
            open_agent: Some(agent_id),
            open_agent_len: None,
        };

        let cwd = self.active_workspace.primary().map(str::to_owned);
        let epoch = self.controller.borrow().runtime.epoch();

        let read = cx
            .background_executor()
            .spawn(async move { workflows::refresh_run(cwd.as_deref(), &session_id, &request) });

        cx.spawn(async move |this, cx| {
            let result = read.await;

            this.update(cx, |this, cx| {
                if this.is_closed() || !this.controller.borrow().runtime.is_current(epoch) {
                    return;
                }

                this.apply_workflow_refresh_results(epoch, vec![result], cx);
            })
            .ok();
        })
        .detach();
    }
}
