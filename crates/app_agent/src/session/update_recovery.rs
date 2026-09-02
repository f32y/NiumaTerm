use std::time::Duration;

use gpui::{App, Context, Task};
use nmt_agent_utils::launcher::AgentCli;
use nmt_agent_utils::update::InstallationKey;
use nmt_i18n::i18n;

use crate::commands::next_session_epoch;
use crate::composer::{ForkState, RewindState};
use crate::profile::agent_launch;
use crate::session::{Backend, RecoveryIdentity, Status};
use crate::{AgentPane, AgentPaneEvent};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoverySnapshot {
    /// `None` for an untouched conversation, which has nothing to resume and
    /// so restarts as a new one rather than failing the update.
    pub identity: Option<RecoveryIdentity>,
    pub profile_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryReadiness {
    Ready(RecoverySnapshot),
    Busy(String),
    MissingIdentity(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestorationReadiness {
    Pending,
    Ready,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UpdateSuspension {
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
    pub fn installation_key(&self) -> Option<InstallationKey> {
        let provider = self.kind.provider_kind()?;
        let launch = agent_launch(&self.profile);
        let launcher = AgentCli::from_launch(&launch, provider.default_executable());
        Some(InstallationKey::derive(provider, &launcher).key)
    }

    /// Assess both quiescence and recoverability before any related backend
    /// is stopped. A blank tab needs no provider identity because restarting
    /// it as another blank conversation loses no conversation state.
    pub fn recovery_readiness(&self, cx: &App) -> RecoveryReadiness {
        if self
            .runtime
            .update_suspension
            .as_ref()
            .is_some_and(|state| !matches!(state, UpdateSuspension::Waiting))
        {
            return RecoveryReadiness::Busy(
                i18n("agent-update-profile-already-updating").replace("{name}", &self.profile.name),
            );
        }
        if matches!(self.runtime.status, Status::Starting | Status::Running)
            || self.prompts.approval_open()
            || self.palette.awaiting_command_turn
            || !self.palette.command_queue.is_empty()
            || !self.turn.queued_user_messages.is_empty()
            || self.branch.rewind.state.is_some()
            || self.branch.fork.state.is_some()
            || self.transcript.read(cx).is_compacting()
            || self
                .runtime
                .backend
                .as_ref()
                .is_some_and(Backend::has_active_operation)
        {
            return RecoveryReadiness::Busy(
                i18n("agent-update-profile-active-work").replace("{name}", &self.profile.name),
            );
        }

        self.recovery_identity_snapshot(cx)
    }

    pub fn recovery_identity_snapshot(&self, cx: &App) -> RecoveryReadiness {
        let identity = if self.transcript.read(cx).is_empty() {
            None
        } else if let Some(identity) = self
            .runtime
            .backend
            .as_ref()
            .and_then(Backend::recovery_identity)
        {
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

    pub fn prepare_update_wait(&mut self, cx: &mut Context<Self>) {
        self.runtime.update_suspension = Some(UpdateSuspension::Waiting);
        cx.notify();
    }

    pub fn cancel_update_wait(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.runtime.update_suspension,
            Some(UpdateSuspension::Waiting)
        ) {
            self.runtime.update_suspension = None;
            cx.notify();
        }
    }

    pub fn stop_active_work_for_update(&mut self, cx: &mut Context<Self>) {
        if self.prompts.approval_open() {
            self.respond_approval("cancel", cx);
        } else {
            self.interrupt(cx);
        }
        self.palette.command_queue.clear();
        self.palette.awaiting_command_turn = false;
        self.publish_queued_user_messages(cx);
        if self
            .branch
            .rewind
            .state
            .as_ref()
            .is_some_and(RewindState::is_picker)
        {
            self.branch.rewind.state = None;
        }
        if self
            .branch
            .fork
            .state
            .as_ref()
            .is_some_and(ForkState::is_picker)
        {
            self.branch.fork.state = None;
        }
        self.transcript
            .update(cx, |transcript, cx| transcript.set_compacting(false, cx));
        self.runtime.update_suspension = Some(UpdateSuspension::Waiting);
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
        self.runtime.epoch = next_session_epoch(self.runtime.epoch);
        self.runtime.update_suspension = Some(UpdateSuspension::Stopping);
        self.runtime.status = Status::Starting;
        cx.emit(AgentPaneEvent::Interrupted);
        cx.notify();

        let Some(mut backend) = self.runtime.backend.take() else {
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
                    this.runtime.backend = Some(backend);
                    this.runtime.update_suspension = None;
                    this.runtime.status = Status::Idle;
                    cx.notify();
                });
            }
            result
        })
    }

    pub fn mark_provider_updating(&mut self, cx: &mut Context<Self>) {
        self.runtime.update_suspension = Some(UpdateSuspension::Updating);
        cx.notify();
    }

    /// The outcome reaches the caller through [`Self::restoration_readiness`]:
    /// the process now comes up on a background thread, so a failure lands
    /// after this returns.
    pub fn restore_after_update(&mut self, snapshot: &RecoverySnapshot, cx: &mut Context<Self>) {
        self.runtime.update_suspension = Some(UpdateSuspension::Reconnecting);
        self.runtime.last_recovery_snapshot = Some(snapshot.clone());
        self.start_session_with_options(
            snapshot.identity.clone(),
            true,
            |this, started, _| {
                if !started {
                    this.runtime.update_suspension = Some(UpdateSuspension::Failed(
                        i18n("agent-update-recovery-restart-failed").to_string(),
                    ));
                }
            },
            cx,
        );
        cx.notify();
    }

    pub(crate) fn retry_update_recovery(&mut self, cx: &mut Context<Self>) {
        if let Some(snapshot) = self.runtime.last_recovery_snapshot.clone() {
            self.restore_after_update(&snapshot, cx);
        }
    }

    pub fn restoration_readiness(&self) -> RestorationReadiness {
        match self.runtime.update_suspension.as_ref() {
            None if self.runtime.status == Status::Idle => RestorationReadiness::Ready,
            Some(UpdateSuspension::Failed(message)) => {
                RestorationReadiness::Failed(message.clone())
            }
            _ => RestorationReadiness::Pending,
        }
    }

    pub fn fail_update_recovery(&mut self, message: String, cx: &mut Context<Self>) {
        self.runtime.update_suspension = Some(UpdateSuspension::Failed(message));
        cx.notify();
    }

    pub(crate) fn start_new_after_update_failure(&mut self, cx: &mut Context<Self>) {
        self.runtime.update_suspension = Some(UpdateSuspension::Reconnecting);
        self.start_session_with_options(
            None,
            true,
            |this, started, _| {
                if !started {
                    this.runtime.update_suspension = Some(UpdateSuspension::Failed(
                        i18n("agent-update-recovery-new-session-failed").to_string(),
                    ));
                }
            },
            cx,
        );
        cx.notify();
    }
}
