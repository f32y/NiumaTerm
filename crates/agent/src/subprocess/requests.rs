//! Request bookkeeping shared by the stdio-driven adapters: how long each
//! class of request may take, the in-flight table a reply is matched
//! against, and the timer task that expires whatever is still waiting.
//! Claude's stream-json control channel and the Codex app-server router are
//! the only callers, and they need all three together.

use std::collections::HashMap;
use std::hash::Hash;
use std::mem;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::time::{Instant as TimerInstant, sleep_until};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestClass {
    Query,
    Mutation,
    Control,
}

impl RequestClass {
    // Deadlines start at admission. A timed out operation can still be waiting
    // in the writer, so expiration cannot establish that it was never applied.
    pub(crate) fn timeout(self) -> Duration {
        Duration::from_secs(match self {
            Self::Query => 30,
            Self::Mutation => 300,
            Self::Control => 15,
        })
    }

    pub(crate) fn timeout_message(self, provider: &str) -> String {
        match self {
            Self::Query => format!("{provider} query timed out. You can retry the query."),
            Self::Mutation | Self::Control => format!(
                "{provider} request timed out. The operation may still complete; its result is unknown. Check its state before retrying."
            ),
        }
    }
}

#[derive(Default)]
struct State {
    next: Option<Instant>,
    stopped: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,

    /// Stores one permit when nobody waits, so a change made between the
    /// task reading the state and suspending still ends that wait.
    changed: Notify,
}

#[derive(Clone)]
pub(crate) struct TimerHandle(Arc<Shared>);

impl TimerHandle {
    pub(crate) fn set(&self, next: Option<Instant>) {
        let mut state = self.0.state.lock();

        if !state.stopped && state.next != next {
            state.next = next;

            self.0.changed.notify_one();
        }
    }

    pub(crate) fn stop(&self) {
        self.0.state.lock().stopped = true;

        self.0.changed.notify_one();
    }
}

pub(crate) struct DeadlineTimer {
    handle: TimerHandle,
}

impl DeadlineTimer {
    pub(crate) fn new(callback: impl Fn() + Send + 'static) -> Self {
        let handle = TimerHandle(Arc::default());

        nmt_runtime::handle().spawn(run_deadlines(handle.clone(), callback));

        Self { handle }
    }

    pub(crate) fn handle(&self) -> TimerHandle {
        self.handle.clone()
    }
}

async fn run_deadlines(worker: TimerHandle, callback: impl Fn()) {
    loop {
        let next = {
            let mut state = worker.0.state.lock();

            if state.stopped {
                break;
            }

            match state.next {
                Some(next) if next <= Instant::now() => {
                    state.next = None;

                    None
                }
                next => Some(next),
            }
        };

        match next {
            // Expired: the callback can re-arm the timer while resolving
            // requests, so it runs without the state lock.
            None => callback(),
            Some(None) => worker.0.changed.notified().await,
            Some(Some(next)) => {
                tokio::select! {
                    () = worker.0.changed.notified() => {}

                    () = sleep_until(TimerInstant::from_std(next)) => {}
                }
            }
        }
    }
}

impl Drop for DeadlineTimer {
    fn drop(&mut self) {
        self.handle.stop();
    }
}

pub(crate) struct PendingRequests<Id, Op> {
    next_id: u64,
    pub(crate) operations: HashMap<Id, Op>,
    closed: bool,
}

impl<Id: Eq + Hash, Op> PendingRequests<Id, Op> {
    pub(crate) fn new(next_id: u64) -> Self {
        Self {
            next_id,
            operations: HashMap::new(),
            closed: false,
        }
    }

    pub(crate) fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;

        self.next_id += 1;

        id
    }

    pub(crate) fn track(&mut self, id: Id, operation: Op) {
        if !self.closed {
            self.operations.insert(id, operation);
        }
    }

    pub(crate) fn finish(&mut self, id: &Id) -> Option<Op> {
        self.operations.remove(id)
    }

    pub(crate) fn close(&mut self) -> HashMap<Id, Op> {
        self.closed = true;

        mem::take(&mut self.operations)
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed
    }

    #[cfg(test)]
    pub(crate) fn next_id(&self) -> u64 {
        self.next_id
    }
}
