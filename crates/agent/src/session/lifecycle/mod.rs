//! Session ownership and transitions, independent of window and transcript updates.
//!
//! Async work carries an epoch. Only this module advances it or admits a
//! completed start, incoming output, or shutdown result into the live session.

use serde_json::Value;

use crate::chat::{Event, SendOutcome};
use crate::session::backend::{Backend, RecoveryIdentity};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Starting,
    Idle,
    Running,
    Exited,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoverySnapshot {
    /// An untouched conversation has nothing to resume.
    pub identity: Option<RecoveryIdentity>,

    pub profile_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestorationReadiness {
    Pending,
    Ready,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateSuspension {
    Waiting,
    Stopping,
    Updating,
    Reconnecting,
    Failed(String),
}

pub enum StartOutcome {
    Installed,
    Failed(String),
    Superseded(Option<Box<Backend>>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterruptOutcome {
    Unavailable,
    Accepted,
    Rejected,
}

pub struct SessionRuntime {
    backend: Option<Backend>,
    epoch: u64,
    status: Status,
    start_failure: Option<String>,
    update_suspension: Option<UpdateSuspension>,
    last_recovery_snapshot: Option<RecoverySnapshot>,
    pending_interrupt: Option<u64>,
}

impl Default for SessionRuntime {
    fn default() -> Self {
        Self {
            backend: None,
            epoch: 0,
            status: Status::Starting,
            start_failure: None,
            update_suspension: None,
            last_recovery_snapshot: None,
            pending_interrupt: None,
        }
    }
}

impl SessionRuntime {
    pub fn status(&self) -> Status {
        self.status
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn is_current(&self, epoch: u64) -> bool {
        self.epoch == epoch
    }

    pub fn backend(&self) -> Option<&Backend> {
        self.backend.as_ref()
    }

    /// Provider-specific operations borrow the backend while epoch and
    /// lifecycle status remain owned by this module.
    pub fn backend_mut(&mut self) -> Option<&mut Backend> {
        self.backend.as_mut()
    }

    pub fn start_failure(&self) -> Option<&str> {
        self.start_failure.as_deref()
    }

    pub fn update_suspension(&self) -> Option<&UpdateSuspension> {
        self.update_suspension.as_ref()
    }

    pub fn last_recovery_snapshot(&self) -> Option<&RecoverySnapshot> {
        self.last_recovery_snapshot.as_ref()
    }

    pub fn begin_start(&mut self) -> u64 {
        if let Some(backend) = self.backend.as_mut() {
            backend.cancel_title_generation();
        }

        self.epoch = self.epoch.wrapping_add(1);
        self.status = Status::Starting;
        self.start_failure = None;
        self.pending_interrupt = None;

        self.epoch
    }

    pub fn install(&mut self, epoch: u64, spawned: Result<Backend, String>) -> StartOutcome {
        if !self.is_current(epoch) {
            return StartOutcome::Superseded(spawned.ok().map(Box::new));
        }

        match spawned {
            Ok(backend) => {
                self.backend = Some(backend);

                StartOutcome::Installed
            }

            Err(message) => {
                self.status = Status::Exited;
                self.start_failure = Some(message.clone());

                StartOutcome::Failed(message)
            }
        }
    }

    pub fn process(&mut self, epoch: u64, message: Value) -> Option<Vec<Event>> {
        if !self.is_current(epoch) {
            return None;
        }

        Some(
            self.backend
                .as_mut()
                .map_or_else(Vec::new, |backend| backend.process(message)),
        )
    }

    pub fn process_exit(&mut self, epoch: u64) -> Option<Vec<Event>> {
        if !self.is_current(epoch) {
            return None;
        }

        Some(
            self.backend
                .as_mut()
                .map_or_else(Vec::new, Backend::process_exit),
        )
    }

    pub fn retire(&mut self) -> Option<Backend> {
        self.begin_start();

        self.backend.take()
    }

    pub fn send(&mut self, send: impl FnOnce(&mut Backend) -> SendOutcome) -> SendOutcome {
        // A replacement can retain the old backend to keep its shared host
        // alive. That retained session must not receive the new draft.
        if !matches!(self.status, Status::Idle | Status::Running)
            || self.update_suspension.is_some()
        {
            return SendOutcome::NotReady;
        }

        self.backend.as_mut().map_or(SendOutcome::NotReady, send)
    }

    pub fn ready(&mut self) {
        // Claude confirms its settings after TurnStarted; that confirmation
        // must not admit overlapping work by making a running turn look idle.
        if self.status != Status::Running {
            self.status = Status::Idle;
        }

        if matches!(self.update_suspension, Some(UpdateSuspension::Reconnecting)) {
            self.update_suspension = None;
        }
    }

    pub fn turn_started(&mut self) {
        self.status = Status::Running;
    }

    pub fn interrupt(&mut self, turn: Option<u64>) -> InterruptOutcome {
        if let Some(turn) = turn {
            self.pending_interrupt = Some(turn);
        }

        match self.backend.as_mut() {
            None => InterruptOutcome::Unavailable,

            Some(backend) => {
                if backend.interrupt() {
                    InterruptOutcome::Accepted
                } else {
                    InterruptOutcome::Rejected
                }
            }
        }
    }

    pub fn turn_completed(&mut self, turn: u64) -> bool {
        let interrupted = self.pending_interrupt.take() == Some(turn);

        if self.status == Status::Running {
            self.status = Status::Idle;
        }

        interrupted
    }

    pub fn clear_turn(&mut self) {
        self.pending_interrupt = None;
    }

    pub fn exited(&mut self, message: &str) {
        self.status = Status::Exited;

        if matches!(self.update_suspension, Some(UpdateSuspension::Reconnecting)) {
            self.update_suspension = Some(UpdateSuspension::Failed(message.to_owned()));
        }
    }

    pub fn begin_conversation_change(&mut self) -> Status {
        let previous = self.status;

        self.status = Status::Starting;

        previous
    }

    pub fn conversation_change_rejected(&mut self, previous: Status) {
        self.status = previous;
    }

    pub fn wait_for_update(&mut self) {
        self.update_suspension = Some(UpdateSuspension::Waiting);
    }

    pub fn cancel_update_wait(&mut self) -> bool {
        if !matches!(self.update_suspension, Some(UpdateSuspension::Waiting)) {
            return false;
        }

        self.update_suspension = None;

        true
    }

    pub fn suspend_for_update(&mut self) -> (u64, Option<Backend>) {
        let backend = self.retire();

        self.update_suspension = Some(UpdateSuspension::Stopping);

        (self.epoch, backend)
    }

    pub fn shutdown_failed(&mut self, epoch: u64, backend: Backend) -> Result<(), Box<Backend>> {
        if !self.is_current(epoch) {
            return Err(Box::new(backend));
        }

        self.backend = Some(backend);
        self.update_suspension = None;
        self.status = Status::Idle;

        Ok(())
    }

    pub fn provider_updating(&mut self) {
        self.update_suspension = Some(UpdateSuspension::Updating);
    }

    pub fn reconnect(&mut self, snapshot: Option<RecoverySnapshot>) {
        if let Some(snapshot) = snapshot {
            self.last_recovery_snapshot = Some(snapshot);
        }

        self.update_suspension = Some(UpdateSuspension::Reconnecting);
    }

    pub fn recovery_failed(&mut self, message: String) {
        self.update_suspension = Some(UpdateSuspension::Failed(message));
    }

    pub fn restoration_readiness(&self) -> RestorationReadiness {
        match self.update_suspension.as_ref() {
            None if self.status == Status::Idle => RestorationReadiness::Ready,

            Some(UpdateSuspension::Failed(message)) => {
                RestorationReadiness::Failed(message.clone())
            }

            _ => RestorationReadiness::Pending,
        }
    }
}

#[cfg(test)]
mod tests;
