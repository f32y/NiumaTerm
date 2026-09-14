use gpui::{App, Context, Entity, TestAppContext};

use nmt_agent::AgentWorkspace;

use nmt_agent::chat::Event;

use crate::agent_tab::session::Backend;

use crate::agent_tab::{AgentPane, GitBranchPoll};

pub(super) fn deliver_session_event(pane: &Entity<AgentPane>, event: Event, cx: &TestAppContext) {
    cx.update(|cx| {
        let host = pane
            .read(cx)
            .agent_session()
            .expect("pane has an execution session");

        host.update(cx, |session, cx| {
            let epoch = session.controller.borrow().runtime.epoch();

            session.on_event(epoch, event, cx);
        });
    });
}

#[test]
fn refresh_state_coalesces_requests_and_updates_presentation() {
    let mut poll = GitBranchPoll::default();

    assert_eq!(poll.presentation(), ("Detecting branch…".into(), 0.48));

    let generation = poll.begin_refresh().unwrap();

    assert!(poll.begin_refresh().is_none());

    poll.complete(generation, Some("main".into()));

    assert_eq!(poll.presentation(), ("main".into(), 0.72));

    let generation = poll.begin_refresh().unwrap();

    poll.complete(generation, None);

    assert_eq!(poll.presentation(), ("No Git branch".into(), 0.48));
}

#[test]
fn changing_directory_discards_in_flight_branch_results() {
    let mut poll = GitBranchPoll::default();

    let initial = poll.begin_refresh().unwrap();

    poll.complete(initial, Some("main".into()));

    let old = poll.begin_refresh().unwrap();

    poll.invalidate();

    assert_eq!(poll.presentation(), ("Detecting branch…".into(), 0.48));

    let current = poll.begin_refresh().unwrap();

    poll.complete(old, Some("main".into()));

    assert_eq!(poll.presentation(), ("Detecting branch…".into(), 0.48));
    assert!(poll.begin_refresh().is_none());

    poll.complete(current, Some("feature/uv-editor".into()));

    poll.complete(old, Some("main".into()));

    assert_eq!(poll.presentation(), ("feature/uv-editor".into(), 0.72));
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
