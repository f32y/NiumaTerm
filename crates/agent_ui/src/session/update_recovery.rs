use std::time::Duration;

use gpui::{App, Context, Task};
use nmt_agent::launcher::AgentCli;
use nmt_agent::session::update_readiness::{ConversationWork, Readiness, prepare_stop};
use nmt_agent::update::InstallationKey;
use nmt_i18n::i18n;

use crate::profile::agent_launch;
use crate::session::{RecoverySnapshot, RestorationReadiness};
use crate::{AgentPane, AgentPaneEvent};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryReadiness {
    Ready(RecoverySnapshot),
    Busy(String),
    MissingIdentity(String),
}

impl AgentPane {
    /// `None` when this tab's harness has no vendor-managed installation, which
    /// is also what keeps such a tab out of every update transaction: it
    /// matches no installation being updated.
    pub fn installation_key(&self) -> Option<InstallationKey> {
        let provider = self.kind.provider_kind()?;
        let launch = agent_launch(&self.profile);
        let launcher = AgentCli::from_launch(&launch, provider.default_executable());

        Some(InstallationKey::derive(provider, &launcher).key)
    }

    /// Assess both quiescence and recoverability before any related backend
    /// is stopped. A blank tab needs no provider identity because restarting
    /// it as another blank conversation loses no conversation state.
    fn update_work(&self, cx: &App) -> ConversationWork {
        ConversationWork {
            approval_open: self.prompts.approval_open(),
            branch_pending: self.branch.holds_composer(),
            compacting: self.transcript.read(cx).is_compacting(),
            empty: self.transcript.read(cx).is_empty(),
        }
    }

    pub fn recovery_readiness(&self, cx: &App) -> RecoveryReadiness {
        self.present_readiness(self.update_work(cx).readiness(
            &self.runtime,
            &self.palette.commands,
            &self.delivery,
        ))
    }

    pub fn recovery_identity_snapshot(&self, cx: &App) -> RecoveryReadiness {
        self.present_readiness(self.update_work(cx).identity(self.runtime.backend()))
    }

    fn present_readiness(&self, readiness: Readiness) -> RecoveryReadiness {
        match readiness {
            Readiness::Ready(identity) => RecoveryReadiness::Ready(RecoverySnapshot {
                identity,
                profile_name: self.profile.name.clone(),
            }),
            Readiness::Updating => RecoveryReadiness::Busy(
                i18n("agent-update-profile-already-updating").replace("{name}", &self.profile.name),
            ),
            Readiness::ActiveWork => RecoveryReadiness::Busy(
                i18n("agent-update-profile-active-work").replace("{name}", &self.profile.name),
            ),
            Readiness::MissingIdentity => RecoveryReadiness::MissingIdentity(
                i18n("agent-update-profile-missing-identity")
                    .replace("{name}", &self.profile.name)
                    .replace("{provider}", self.kind.display()),
            ),
        }
    }

    pub fn prepare_update_wait(&mut self, cx: &mut Context<Self>) {
        self.runtime.wait_for_update();
        cx.notify();
    }

    pub fn cancel_update_wait(&mut self, cx: &mut Context<Self>) {
        if self.runtime.cancel_update_wait() {
            cx.notify();
        }
    }

    pub fn stop_active_work_for_update(&mut self, cx: &mut Context<Self>) {
        if self.prompts.approval_open() {
            self.respond_approval("cancel", cx);
        } else {
            self.interrupt(cx);
        }

        prepare_stop(&mut self.palette.commands, &mut self.delivery);
        self.publish_queued_user_messages(cx);

        self.cancel_branch_picker(cx);

        self.transcript
            .update(cx, |transcript, cx| transcript.set_compacting(false, cx));
        self.runtime.wait_for_update();
        cx.notify();
    }

    /// Detach the backend before shutdown so its EOF cannot be mistaken for
    /// an unexpected pane exit. The transcript, draft, selection, scroll, and
    /// thread controls remain owned by this entity throughout the operation.
    pub fn suspend_for_update(
        &mut self,
        force: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let (epoch, backend) = self.runtime.suspend_for_update();

        self.history_ui.invalidate_filesystem_history();
        cx.emit(AgentPaneEvent::Interrupted);
        cx.notify();

        let Some(mut backend) = backend else {
            return Task::ready(Ok(()));
        };

        let worker = cx.background_executor().spawn(async move {
            let result = backend.shutdown(Duration::from_secs(5), force);
            (backend, result)
        });

        cx.spawn(async move |this, cx| {
            let (backend, result) = worker.await;

            if result.is_err() {
                let _ = this.update(cx, |this, cx| {
                    if let Err(mut orphan) = this.runtime.shutdown_failed(epoch, backend) {
                        cx.background_executor()
                            .spawn(async move {
                                let _ = orphan.shutdown(Duration::from_secs(5), true);
                            })
                            .detach();
                    }

                    cx.notify();
                });
            }

            result
        })
    }

    pub fn mark_provider_updating(&mut self, cx: &mut Context<Self>) {
        self.runtime.provider_updating();
        cx.notify();
    }

    /// The outcome reaches the caller through [`Self::restoration_readiness`]:
    /// the process now comes up on a background thread, so a failure lands
    /// after this returns.
    pub fn restore_after_update(&mut self, snapshot: &RecoverySnapshot, cx: &mut Context<Self>) {
        self.runtime.reconnect(Some(snapshot.clone()));
        self.start_session_with_options(
            snapshot.identity.clone(),
            true,
            |this, started, _| {
                if !started {
                    this.runtime
                        .recovery_failed(i18n("agent-update-recovery-restart-failed").to_string());
                }
            },
            cx,
        );
        cx.notify();
    }

    pub(crate) fn retry_update_recovery(&mut self, cx: &mut Context<Self>) {
        if let Some(snapshot) = self.runtime.last_recovery_snapshot().cloned() {
            self.restore_after_update(&snapshot, cx);
        }
    }

    pub fn restoration_readiness(&self) -> RestorationReadiness {
        self.runtime.restoration_readiness()
    }

    pub fn fail_update_recovery(&mut self, message: String, cx: &mut Context<Self>) {
        self.runtime.recovery_failed(message);
        cx.notify();
    }

    pub(crate) fn start_new_after_update_failure(&mut self, cx: &mut Context<Self>) {
        self.runtime.reconnect(None);
        self.start_session_with_options(
            None,
            true,
            |this, started, _| {
                if !started {
                    this.runtime.recovery_failed(
                        i18n("agent-update-recovery-new-session-failed").to_string(),
                    );
                }
            },
            cx,
        );
        cx.notify();
    }
}
