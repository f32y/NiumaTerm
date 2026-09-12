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

pub use nmt_agent::session::workflows::OpenWorkflowAgent;

use std::cell::{Cell, Ref};
use std::rc::Rc;

use gpui::Context;
use nmt_agent::session::workflows::WorkflowReader;
use nmt_agent::workflow::WorkflowRun;

use crate::agent_tab::AgentPane;
use crate::agent_tab::capabilities::AgentCapabilities as _;
use crate::agent_tab::session::Backend;

#[derive(Default)]
pub(super) struct WorkflowUi {
    visible: bool,
    reader: Option<WorkflowReader>,
    interest: Option<Rc<Cell<usize>>>,
}

impl WorkflowUi {
    fn release(&mut self) {
        if let Some(readers) = self.interest.take() {
            readers.set(readers.get().saturating_sub(1));
        }
    }
}

impl Drop for WorkflowUi {
    fn drop(&mut self) {
        self.release();
    }
}

impl AgentPane {
    /// Stop polling the old conversation when its state is cleared.
    pub(super) fn clear_workflows(&mut self) {
        self.workflows.reader = None;
    }

    /// Show or hide the view. Refreshing follows visibility, so this is what
    /// starts and stops the one-second poll.
    pub fn set_workflows_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.workflows.visible == visible {
            return;
        }

        self.workflows.visible = visible;

        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, cx| {
                if visible {
                    host.workflow_readers.set(host.workflow_readers.get() + 1);
                    self.workflows.interest = Some(host.workflow_readers.clone());
                    host.restore_workflows(cx);
                } else {
                    self.workflows.release();
                }

                host.sync_workflow_refresh(cx);
            });
        }

        cx.notify();
    }

    /// Session id when this pane runs a harness that reports workflows, which
    /// is what scopes runs to the conversation they belong to.
    pub fn workflow_session_id(&self) -> Option<String> {
        if !self.kind.caps().workflows {
            return None;
        }

        self.session
            .borrow()
            .runtime
            .backend()
            .and_then(Backend::session_id)
            .map(str::to_owned)
    }

    /// Runs of the scoped session, in provider order.
    pub fn workflow_runs(&self) -> Ref<'_, [WorkflowRun]> {
        Ref::map(self.session.borrow(), |session| session.workflows.runs())
    }

    /// Agents of this tab the provider currently reports as running.
    pub fn running_workflow_agents(&self) -> usize {
        self.session.borrow().workflows.running_agents()
    }

    /// The agent conversation the user has open, if any.
    pub fn open_workflow_conversation(&self) -> Option<Ref<'_, OpenWorkflowAgent>> {
        let reader = self.workflows.reader.as_ref()?;

        Ref::filter_map(self.session.borrow(), |session| {
            session.workflows.conversation(&reader.key.0, &reader.key.1)
        })
        .ok()
    }

    /// Open one agent's conversation, reading it immediately rather than
    /// waiting for the next tick.
    pub fn open_workflow_agent(&mut self, task_id: &str, agent_id: &str, cx: &mut Context<Self>) {
        self.workflows.reader = Some(
            self.session
                .borrow_mut()
                .workflows
                .open_agent(task_id, agent_id),
        );

        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, cx| {
                host.read_open_workflow_agent(task_id, agent_id, cx)
            });
        }

        cx.notify();
    }

    pub fn close_workflow_agent(&mut self, cx: &mut Context<Self>) {
        self.workflows.reader = None;

        cx.notify();
    }
}
