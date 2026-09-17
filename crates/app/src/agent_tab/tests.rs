use gpui::{App, Context, Entity, TestAppContext};
use nmt_agent::AgentWorkspace;
use nmt_agent::chat::Event;

use crate::agent_tab::AgentPane;
use crate::agent_tab::session::Backend;

pub(super) fn deliver_session_event(pane: &Entity<AgentPane>, event: Event, cx: &TestAppContext) {
    cx.update(|cx| {
        let host = pane
            .read(cx)
            .agent_session()
            .expect("pane has an execution session");

        host.update(cx, |session, cx| {
            let epoch = session.controller.borrow().runtime().epoch();

            session.on_event(epoch, event, cx);
        });
    });
}

impl AgentPane {
    /// The directories the running conversation was started with.
    pub(super) fn active_workspace<'a>(&self, cx: &'a App) -> Option<&'a AgentWorkspace> {
        Some(&self.host.upgrade()?.read(cx).active_workspace)
    }

    pub(super) fn install_started_session(
        &mut self,
        spawned: Result<Backend, String>,
        epoch: u64,
        name: &'static str,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        self.host
            .upgrade()?
            .update(cx, |host, cx| host.install(spawned, epoch, name, cx))
    }

    pub(super) fn stop_for_output_failure(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, cx| host.stop_for_output_failure(error, cx));
        }
    }

    pub(crate) fn restore_question_drafts(&mut self) {
        self.session.borrow_mut().restore_questions();

        self.prompts.reset_editors();
    }
}
