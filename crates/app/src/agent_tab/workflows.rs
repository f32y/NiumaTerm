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

use std::cell::{Cell, Ref, RefCell};
use std::rc::Rc;

use gpui::{Context, WeakEntity};
use nmt_agent::session::controller::SessionController;
use nmt_agent::session::workflows::{OpenWorkflowAgent, WorkflowReader};

use crate::agent_tab::AgentPane;
use crate::agent_tab::execution::AgentSession;

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

    /// Stop polling the old conversation when its state is cleared.
    pub(super) fn clear_workflows(&mut self) {
        self.reader = None;
    }

    /// Show or hide the view. Refreshing follows visibility, so this is what
    /// starts and stops the one-second poll.
    pub fn set_workflows_visible(
        &mut self,
        host: &WeakEntity<AgentSession>,
        visible: bool,
        cx: &mut Context<AgentPane>,
    ) {
        if self.visible == visible {
            return;
        }

        self.visible = visible;

        if let Some(host) = host.upgrade() {
            host.update(cx, |host, cx| {
                if visible {
                    host.workflow_refresh
                        .readers
                        .set(host.workflow_refresh.readers.get() + 1);

                    self.interest = Some(host.workflow_refresh.readers.clone());

                    host.restore_workflows(cx);
                } else {
                    self.release();
                }

                host.sync_workflow_refresh(cx);
            });
        }

        cx.notify();
    }

    /// The agent conversation the user has open, if any.
    pub fn open_workflow_conversation<'a>(
        &self,
        session: &'a RefCell<SessionController>,
    ) -> Option<Ref<'a, OpenWorkflowAgent>> {
        let reader = self.reader.as_ref()?;

        Ref::filter_map(session.borrow(), |session| {
            session.workflows.conversation(&reader.key.0, &reader.key.1)
        })
        .ok()
    }

    /// Open one agent's conversation, reading it immediately rather than
    /// waiting for the next tick.
    pub fn open_workflow_agent(
        &mut self,
        session: &RefCell<SessionController>,
        host: &WeakEntity<AgentSession>,
        task_id: &str,
        agent_id: &str,
        cx: &mut Context<AgentPane>,
    ) {
        self.reader = Some(session.borrow_mut().workflows.open_agent(task_id, agent_id));

        if let Some(host) = host.upgrade() {
            host.update(cx, |host, cx| {
                host.read_open_workflow_agent(task_id, agent_id, cx)
            });
        }

        cx.notify();
    }

    pub fn close_workflow_agent(&mut self, cx: &mut Context<AgentPane>) {
        self.reader = None;

        cx.notify();
    }
}

impl Drop for WorkflowUi {
    fn drop(&mut self) {
        self.release();
    }
}
