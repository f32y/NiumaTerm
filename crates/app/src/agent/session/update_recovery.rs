use nmt_i18n::i18n;

use crate::agent::session::{Backend, RecoveryIdentity, Status};
use crate::agent::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecoverySnapshot {
    /// `None` for an untouched conversation, which has nothing to resume and
    /// so restarts as a new one rather than failing the update.
    pub(crate) identity: Option<RecoveryIdentity>,
    pub(crate) profile_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryReadiness {
    Ready(RecoverySnapshot),
    Busy(String),
    MissingIdentity(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RestorationReadiness {
    Pending,
    Ready,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::agent) enum UpdateSuspension {
    Waiting,
    Stopping,
    Updating,
    Reconnecting,
    Failed(String),
}

impl AgentPane {
    /// `None` when this tab's harness has no vendor-managed installation, which
    /// is also what keeps such a tab out of every update transaction: it
    /// matches no installation being updated.
    pub(crate) fn installation_key(&self) -> Option<InstallationKey> {
        let provider = self.kind.provider_kind()?;
        let launch = agent_launch(&self.profile);
        let launcher = AgentCli::from_launch(&launch, provider.default_executable());
        Some(InstallationKey::derive(provider, &launcher).key)
    }

    /// Assess both quiescence and recoverability before any related backend
    /// is stopped. A blank tab needs no provider identity because restarting
    /// it as another blank conversation loses no conversation state.
    pub(crate) fn recovery_readiness(&self, cx: &App) -> RecoveryReadiness {
        if self
            .update_suspension
            .as_ref()
            .is_some_and(|state| !matches!(state, UpdateSuspension::Waiting))
        {
            return RecoveryReadiness::Busy(
                i18n("agent-update-profile-already-updating").replace("{name}", &self.profile.name),
            );
        }
        if matches!(self.status, Status::Starting | Status::Running)
            || self.pending_approval.is_some()
            || self.palette.awaiting_command_turn
            || !self.palette.command_queue.is_empty()
            || !self.queued_user_messages.is_empty()
            || self.rewind.state.is_some()
            || self.transcript.read(cx).is_compacting()
            || self
                .session
                .as_ref()
                .is_some_and(Backend::has_active_operation)
        {
            return RecoveryReadiness::Busy(
                i18n("agent-update-profile-active-work").replace("{name}", &self.profile.name),
            );
        }

        self.recovery_identity_snapshot(cx)
    }

    pub(crate) fn recovery_identity_snapshot(&self, cx: &App) -> RecoveryReadiness {
        let identity = if self.transcript.read(cx).is_empty() {
            None
        } else if let Some(identity) = self.session.as_ref().and_then(Backend::recovery_identity) {
            Some(identity)
        } else {
            return RecoveryReadiness::MissingIdentity(
                i18n("agent-update-profile-missing-identity")
                    .replace("{name}", &self.profile.name)
                    .replace("{provider}", self.kind.display()),
            );
        };

        RecoveryReadiness::Ready(RecoverySnapshot {
            identity,
            profile_name: self.profile.name.clone(),
        })
    }

    pub(crate) fn prepare_update_wait(&mut self, cx: &mut Context<Self>) {
        self.update_suspension = Some(UpdateSuspension::Waiting);
        cx.notify();
    }

    pub(crate) fn cancel_update_wait(&mut self, cx: &mut Context<Self>) {
        if matches!(self.update_suspension, Some(UpdateSuspension::Waiting)) {
            self.update_suspension = None;
            cx.notify();
        }
    }

    pub(crate) fn stop_active_work_for_update(&mut self, cx: &mut Context<Self>) {
        if self.pending_approval.is_some() {
            self.respond_approval("cancel", cx);
        } else {
            self.interrupt(cx);
        }
        self.palette.command_queue.clear();
        self.palette.awaiting_command_turn = false;
        self.publish_queued_user_messages(cx);
        if self
            .rewind
            .state
            .as_ref()
            .is_some_and(RewindState::is_picker)
        {
            self.rewind.state = None;
        }
        self.transcript
            .update(cx, |transcript, cx| transcript.set_compacting(false, cx));
        self.update_suspension = Some(UpdateSuspension::Waiting);
        cx.notify();
    }

    /// Detach the backend before shutdown so its EOF cannot be mistaken for
    /// an unexpected pane exit. The transcript, draft, selection, scroll, and
    /// thread controls remain owned by this entity throughout the operation.
    pub(crate) fn suspend_for_update(
        &mut self,
        force: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        self.session_epoch = next_session_epoch(self.session_epoch);
        self.update_suspension = Some(UpdateSuspension::Stopping);
        self.status = Status::Starting;
        cx.emit(AgentPaneEvent::Interrupted);
        cx.notify();

        let Some(mut backend) = self.session.take() else {
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
                    this.session = Some(backend);
                    this.update_suspension = None;
                    this.status = Status::Idle;
                    cx.notify();
                });
            }
            result
        })
    }

    pub(crate) fn mark_provider_updating(&mut self, cx: &mut Context<Self>) {
        self.update_suspension = Some(UpdateSuspension::Updating);
        cx.notify();
    }

    /// The outcome reaches the caller through [`Self::restoration_readiness`]:
    /// the process now comes up on a background thread, so a failure lands
    /// after this returns.
    pub(crate) fn restore_after_update(
        &mut self,
        snapshot: &RecoverySnapshot,
        cx: &mut Context<Self>,
    ) {
        self.update_suspension = Some(UpdateSuspension::Reconnecting);
        self.last_recovery_snapshot = Some(snapshot.clone());
        self.start_session_with_options(
            snapshot.identity.clone(),
            true,
            |this, started, _| {
                if !started {
                    this.update_suspension = Some(UpdateSuspension::Failed(
                        i18n("agent-update-recovery-restart-failed").to_string(),
                    ));
                }
            },
            cx,
        );
        cx.notify();
    }

    pub(in crate::agent) fn retry_update_recovery(&mut self, cx: &mut Context<Self>) {
        if let Some(snapshot) = self.last_recovery_snapshot.clone() {
            self.restore_after_update(&snapshot, cx);
        }
    }

    pub(crate) fn restoration_readiness(&self) -> RestorationReadiness {
        match self.update_suspension.as_ref() {
            None if self.status == Status::Idle => RestorationReadiness::Ready,
            Some(UpdateSuspension::Failed(message)) => {
                RestorationReadiness::Failed(message.clone())
            }
            _ => RestorationReadiness::Pending,
        }
    }

    pub(crate) fn fail_update_recovery(&mut self, message: String, cx: &mut Context<Self>) {
        self.update_suspension = Some(UpdateSuspension::Failed(message));
        cx.notify();
    }

    pub(in crate::agent) fn start_new_after_update_failure(&mut self, cx: &mut Context<Self>) {
        self.update_suspension = Some(UpdateSuspension::Reconnecting);
        self.start_session_with_options(
            None,
            true,
            |this, started, _| {
                if !started {
                    this.update_suspension = Some(UpdateSuspension::Failed(
                        i18n("agent-update-recovery-new-session-failed").to_string(),
                    ));
                }
            },
            cx,
        );
        cx.notify();
    }
}
