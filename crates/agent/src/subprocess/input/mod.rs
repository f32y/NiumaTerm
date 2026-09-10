use std::collections::VecDeque;
#[cfg(test)]
use std::iter::from_fn;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Weak, mpsc};

use parking_lot::{Condvar, Mutex};
use serde_json::Value;
use thiserror::Error;

use crate::message_memory::retained_bytes as estimated_bytes;

const MAX_MESSAGES: usize = 64;
const MAX_BYTES: usize = 32 * 1024 * 1024;
const RESERVED_MESSAGES: usize = 8;
const RESERVED_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy)]
pub(crate) enum InputClass {
    Normal,
    Control,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum InputError {
    #[error("Agent input exceeds the {limit} byte message budget")]
    TooLarge { limit: usize },
    #[error("Agent input queue has no free message slots; retry shortly")]
    MessageLimit,
    #[error("Agent input queue has no free byte budget; retry shortly")]
    ByteLimit,
    #[error("The agent input is closed")]
    Closed,
}

#[derive(Default)]
struct Budget {
    messages: usize,
    bytes: usize,
}

struct Reservation {
    budget: Arc<Mutex<Budget>>,
    messages: usize,
    bytes: usize,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut budget = self.budget.lock();
        budget.messages -= self.messages;
        budget.bytes -= self.bytes;
    }
}

pub(super) struct QueuedInput {
    pub(super) messages: Vec<Value>,
    pub(super) ticket: InputTicket,
    // The writer retains this reservation until every line has finished.
    // Removing a blocked write from the channel must not free its budget.
    _reservation: Reservation,
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
    /// used to start writes. Started writes retain their reservation until I/O
    /// finishes; a cancelled item releases memory and slots before returning.
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
    budget: Arc<Mutex<Budget>>,
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

    #[cfg(test)]
    pub(super) fn try_iter(&self) -> impl Iterator<Item = QueuedInput> + '_ {
        from_fn(|| self.try_recv().ok())
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
                budget: Arc::new(Mutex::new(Budget::default())),
            },
            InputReceiver { queue },
        )
    }

    #[cfg(test)]
    pub(super) fn submit(&self, messages: Vec<Value>, class: InputClass) -> Result<(), InputError> {
        self.submit_tracked(messages, class).map(|_| ())
    }

    pub(super) fn submit_tracked(
        &self,
        messages: Vec<Value>,
        class: InputClass,
    ) -> Result<InputTicket, InputError> {
        let mut ticket = InputTicket::new(
            messages.len() > 1 || messages.iter().any(|message| message["type"] == "user"),
        );

        ticket.queue = Arc::downgrade(&self.queue);

        if messages.is_empty() {
            return Ok(ticket);
        }

        let (max_messages, max_bytes) = match class {
            InputClass::Normal => (MAX_MESSAGES - RESERVED_MESSAGES, MAX_BYTES - RESERVED_BYTES),
            InputClass::Control => (MAX_MESSAGES, MAX_BYTES),
        };

        let bytes = messages.iter().fold(0usize, |total, message| {
            total.saturating_add(estimated_bytes(message))
        });

        if bytes > max_bytes {
            return Err(InputError::TooLarge { limit: max_bytes });
        }

        let mut state = self.queue.state.lock();

        if !state.receiver_open {
            return Err(InputError::Closed);
        }

        let reservation = {
            let mut budget = self.budget.lock();

            if messages.len() > max_messages.saturating_sub(budget.messages) {
                return Err(InputError::MessageLimit);
            }

            if bytes > max_bytes.saturating_sub(budget.bytes) {
                return Err(InputError::ByteLimit);
            }

            budget.messages += messages.len();
            budget.bytes += bytes;

            Reservation {
                budget: Arc::clone(&self.budget),
                messages: messages.len(),
                bytes,
            }
        };

        state.pending.push_back(QueuedInput {
            messages,
            ticket: ticket.clone(),
            _reservation: reservation,
        });
        self.queue.ready.notify_one();

        Ok(ticket)
    }
}

#[cfg(test)]
mod tests;
