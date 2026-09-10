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
    // The writer retains this reservation until every line has finished.
    // Removing a blocked write from the channel must not free its budget.
    _reservation: Reservation,
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

    pub(super) fn submit(&self, messages: Vec<Value>, class: InputClass) -> Result<(), InputError> {
        if messages.is_empty() {
            return Ok(());
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
                _reservation: reservation,
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => InputError::MessageLimit,
                mpsc::TrySendError::Disconnected(_) => InputError::Closed,
            })
    }
}

#[cfg(test)]
mod tests;
