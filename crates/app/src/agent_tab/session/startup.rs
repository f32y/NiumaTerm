//! View reactions to session startup.

use gpui::Context;
#[cfg(test)]
use nmt_agent::session::Backend;
use nmt_agent::session::RecoveryIdentity;
use nmt_agent::session::lifecycle::Status;

use crate::agent_tab::AgentPane;
use crate::agent_tab::profile::AgentKind;

impl AgentPane {
    pub(crate) fn shows_start_overlay(&self) -> bool {
        self.session.borrow().runtime.status() == Status::Starting
    }

    pub(crate) fn start_session(&mut self, resume: Option<String>, cx: &mut Context<Self>) {
        self.start_session_with_options(
            resume.map(|id| RecoveryIdentity::new(AgentKind::Claude, id)),
            false,
            |_, _, _| {},
            cx,
        );
    }

    pub(crate) fn start_session_with_options(
        &mut self,
        recovery: Option<RecoveryIdentity>,
        preserve_settings: bool,
        on_result: impl FnOnce(&mut Self, bool, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        if !self.binding.is_current() {
            return;
        }

        let Some(host) = self.host.upgrade() else {
            return;
        };

        self.history_ui.invalidate_filesystem_history();
        self.palette.skill_catalog = None;
        self.palette.skill_binding = None;

        let pane = cx.entity().downgrade();

        host.update(cx, |host, cx| {
            host.workspace = self.workspace.clone();

            host.start(
                recovery,
                preserve_settings,
                move |started, cx| {
                    let _ = pane.update(cx, |pane, cx| {
                        if pane.binding.is_current() {
                            on_result(pane, started, cx);

                            cx.notify();
                        }
                    });
                },
                cx,
            );
        });

        self.profile = host.read(cx).profile.clone();
        self.active_workspace = host.read(cx).active_workspace.clone();

        self.prompts
            .release_secret_editors(&self.session.borrow().input);

        if !self.session.borrow().branch.holds_composer() {
            self.branch.clear();
        }

        cx.notify();
    }

    #[cfg(test)]
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

    #[cfg(test)]
    pub(super) fn stop_for_output_failure(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(host) = self.host.upgrade() {
            host.update(cx, |host, cx| host.stop_for_output_failure(error, cx));
        }
    }
}
