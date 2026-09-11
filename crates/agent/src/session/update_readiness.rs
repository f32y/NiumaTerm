use crate::session::commands::CommandQueue;
use crate::session::delivery::MessageDelivery;
use crate::session::lifecycle::{SessionRuntime, Status, UpdateSuspension};
use crate::session::{Backend, RecoveryIdentity};

pub enum Readiness {
    Ready(Option<RecoveryIdentity>),
    Updating,
    ActiveWork,
    MissingIdentity,
}

pub struct ConversationWork {
    pub approval_open: bool,
    pub branch_pending: bool,
    pub compacting: bool,
    pub empty: bool,
}

impl ConversationWork {
    pub fn readiness(
        &self,
        runtime: &SessionRuntime,
        commands: &CommandQueue,
        delivery: &MessageDelivery,
    ) -> Readiness {
        if runtime
            .update_suspension()
            .is_some_and(|state| !matches!(state, UpdateSuspension::Waiting))
        {
            return Readiness::Updating;
        }

        if matches!(runtime.status(), Status::Starting | Status::Running)
            || self.approval_open
            || self.branch_pending
            || self.compacting
            || commands.awaiting_turn
            || !commands.queue.is_empty()
            || !delivery.pending().is_empty()
            || runtime.backend().is_some_and(Backend::has_active_operation)
        {
            return Readiness::ActiveWork;
        }

        self.identity(runtime.backend())
    }

    pub fn identity(&self, backend: Option<&Backend>) -> Readiness {
        if self.empty {
            return Readiness::Ready(None);
        }

        match backend.and_then(Backend::recovery_identity) {
            Some(identity) => Readiness::Ready(Some(identity)),
            None => Readiness::MissingIdentity,
        }
    }
}

pub fn prepare_stop(commands: &mut CommandQueue, delivery: &mut MessageDelivery) {
    commands.clear();
    delivery.stopping_for_update();
}
