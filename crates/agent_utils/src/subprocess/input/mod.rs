use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, mpsc};

use parking_lot::Mutex;
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
}

impl InputTicket {
    #[cfg(test)]
    pub(crate) fn queued_for_test(batch: bool) -> Self {
        Self::new(batch)
    }

    fn new(batch: bool) -> Self {
        Self {
            state: Arc::new(AtomicU8::new(0)),
            batch,
        }
    }

    pub(super) fn begin(&self) -> bool {
        self.state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// True only when no byte from this item can have been written. Every
    /// request in a batch shares the same marker and is cancelled together.
    pub(crate) fn cancel(&self) -> bool {
        matches!(
            self.state
                .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire),
            Ok(_) | Err(2)
        )
    }

    pub(crate) fn is_batch(&self) -> bool {
        self.batch
    }
}

pub(super) struct InputQueue {
    sender: mpsc::SyncSender<QueuedInput>,
    budget: Arc<Mutex<Budget>>,
}

impl InputQueue {
    pub(super) fn new() -> (Self, mpsc::Receiver<QueuedInput>) {
        let (sender, receiver) = mpsc::sync_channel(MAX_MESSAGES);
        (
            Self {
                sender,
                budget: Arc::new(Mutex::new(Budget::default())),
            },
            receiver,
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
        let ticket = InputTicket::new(
            messages.len() > 1 || messages.iter().any(|message| message["type"] == "user"),
        );
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
        self.sender
            .try_send(QueuedInput {
                messages,
                ticket: ticket.clone(),
                _reservation: reservation,
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => InputError::MessageLimit,
                mpsc::TrySendError::Disconnected(_) => InputError::Closed,
            })?;
        Ok(ticket)
    }
}

#[cfg(test)]
mod tests;
