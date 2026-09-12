#[cfg(test)]
#[path = "input_tests.rs"]
mod input_tests;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Weak, mpsc};

use parking_lot::{Condvar, Mutex};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum InputError {
    #[error("The agent input is closed")]
    Closed,
}

pub(super) struct QueuedInput {
    pub(super) messages: Vec<Value>,
    pub(super) ticket: InputTicket,
}

#[derive(Clone)]
pub(crate) struct InputTicket {
    state: Arc<AtomicU8>,
    batch: bool,
    queue: Weak<Queue>,
}

impl InputTicket {
    #[cfg(test)]
    pub(crate) fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) == 2
    }

    #[cfg(test)]
    pub(crate) fn queued_for_test(batch: bool) -> Self {
        Self::new(batch)
    }

    fn new(batch: bool) -> Self {
        Self {
            state: Arc::new(AtomicU8::new(0)),
            batch,
            queue: Weak::new(),
        }
    }

    /// Cancellation removes the pending payload while holding the same lock
    /// used to start writes, so cancellation cannot split a started batch.
    pub(crate) fn cancel(&self) -> bool {
        let Some(queue) = self.queue.upgrade() else {
            return matches!(
                self.state
                    .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire),
                Ok(_) | Err(2)
            );
        };

        let mut state = queue.state.lock();

        if self.state.load(Ordering::Acquire) == 1 {
            return false;
        }

        if let Some(index) = state
            .pending
            .iter()
            .position(|input| Arc::ptr_eq(&input.ticket.state, &self.state))
        {
            state.pending.remove(index);
        }

        self.state.store(2, Ordering::Release);

        true
    }

    pub(crate) fn is_batch(&self) -> bool {
        self.batch
    }
}

struct QueueState {
    pending: VecDeque<QueuedInput>,
    sender_open: bool,
    receiver_open: bool,
}

struct Queue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

pub(super) struct InputQueue {
    queue: Arc<Queue>,
}

pub(super) struct InputReceiver {
    queue: Arc<Queue>,
}

impl InputReceiver {
    pub(super) fn recv(&self) -> Result<QueuedInput, mpsc::RecvError> {
        let mut state = self.queue.state.lock();

        loop {
            if let Some(input) = state.pending.pop_front() {
                input.ticket.state.store(1, Ordering::Release);

                return Ok(input);
            }

            if !state.sender_open {
                return Err(mpsc::RecvError);
            }

            self.queue.ready.wait(&mut state);
        }
    }

    #[cfg(test)]
    pub(super) fn try_recv(&self) -> Result<QueuedInput, mpsc::TryRecvError> {
        let mut state = self.queue.state.lock();

        if let Some(input) = state.pending.pop_front() {
            input.ticket.state.store(1, Ordering::Release);

            Ok(input)
        } else if state.sender_open {
            Err(mpsc::TryRecvError::Empty)
        } else {
            Err(mpsc::TryRecvError::Disconnected)
        }
    }
}

impl Iterator for InputReceiver {
    type Item = QueuedInput;

    fn next(&mut self) -> Option<Self::Item> {
        self.recv().ok()
    }
}

impl Drop for InputReceiver {
    fn drop(&mut self) {
        let mut state = self.queue.state.lock();

        state.receiver_open = false;

        for input in state.pending.drain(..) {
            input.ticket.state.store(2, Ordering::Release);
        }
    }
}

impl Drop for InputQueue {
    fn drop(&mut self) {
        self.queue.state.lock().sender_open = false;
        self.queue.ready.notify_one();
    }
}

impl InputQueue {
    pub(super) fn new() -> (Self, InputReceiver) {
        let queue = Arc::new(Queue {
            state: Mutex::new(QueueState {
                pending: VecDeque::new(),
                sender_open: true,
                receiver_open: true,
            }),
            ready: Condvar::new(),
        });

        (
            Self {
                queue: Arc::clone(&queue),
            },
            InputReceiver { queue },
        )
    }

    #[cfg(test)]
    pub(super) fn submit(&self, messages: Vec<Value>) -> Result<(), InputError> {
        self.submit_tracked(messages).map(|_| ())
    }

    pub(super) fn submit_tracked(&self, messages: Vec<Value>) -> Result<InputTicket, InputError> {
        let mut ticket = InputTicket::new(
            messages.len() > 1 || messages.iter().any(|message| message["type"] == "user"),
        );

        ticket.queue = Arc::downgrade(&self.queue);

        if messages.is_empty() {
            return Ok(ticket);
        }

        let mut state = self.queue.state.lock();

        if !state.receiver_open {
            return Err(InputError::Closed);
        }

        state.pending.push_back(QueuedInput {
            messages,
            ticket: ticket.clone(),
        });

        self.queue.ready.notify_one();

        Ok(ticket)
    }
}
