use std::sync::Arc;
use std::time::Duration;

use futures::channel::mpsc;
use gpui::Context;
use nmt_agent_utils::chat::Event;
use nmt_agent_utils::message_memory::{OUTPUT_FAILURE_METHOD, retained_bytes};
use parking_lot::Mutex;
use serde_json::Value;

use crate::AgentPane;

const MAX_MESSAGES: usize = 1024;
const MAX_BYTES: usize = 32 * 1024 * 1024;

impl AgentPane {
    pub(super) fn stop_for_output_failure(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(mut backend) = self.runtime.backend.take() {
            for event in backend.process_exit() {
                self.apply_event(event, cx);
            }
            cx.background_executor()
                .spawn(async move {
                    let _ = backend.shutdown(Duration::from_millis(250), true);
                })
                .detach();
        }
        self.apply_event(
            Event::Error {
                message: error,
                fatal: true,
            },
            cx,
        );
        cx.notify();
    }
}

#[derive(Default)]
struct Budget {
    messages: usize,
    bytes: usize,
}

pub(super) struct Message {
    value: Option<Value>,
    bytes: usize,
    budget: Arc<Mutex<Budget>>,
}

impl Message {
    pub(super) fn take(&mut self) -> Value {
        self.value.take().expect("queued message is consumed once")
    }
}

impl Drop for Message {
    fn drop(&mut self) {
        let mut budget = self.budget.lock();
        budget.messages -= 1;
        budget.bytes -= self.bytes;
    }
}

pub(super) struct Sender {
    sender: Mutex<Option<mpsc::UnboundedSender<Result<Message, String>>>>,
    budget: Arc<Mutex<Budget>>,
}

pub(super) fn channel() -> (Sender, mpsc::UnboundedReceiver<Result<Message, String>>) {
    let (sender, receiver) = mpsc::unbounded();
    (
        Sender {
            sender: Mutex::new(Some(sender)),
            budget: Arc::new(Mutex::new(Budget::default())),
        },
        receiver,
    )
}

impl Sender {
    pub(super) fn send(&self, value: Value) {
        // Serialize admission and terminal failure so no later producer can
        // publish a message after the stream has become incomplete.
        let mut sender = self.sender.lock();
        let Some(tx) = sender.as_ref() else { return };
        let bytes = retained_bytes(&value);
        let mut budget = self.budget.lock();
        let error = if value["method"] == OUTPUT_FAILURE_METHOD {
            Some(
                value["params"]["message"]
                    .as_str()
                    .unwrap_or("Agent output failed")
                    .to_string(),
            )
        } else if budget.messages >= MAX_MESSAGES || bytes > MAX_BYTES.saturating_sub(budget.bytes)
        {
            Some("Agent output exceeded the receive budget; the session was stopped to avoid losing conversation messages. Reopen it from history.".to_string())
        } else {
            None
        };
        if let Some(error) = error {
            drop(budget);
            let _ = tx.unbounded_send(Err(error));
            sender.take();
            return;
        }
        budget.messages += 1;
        budget.bytes += bytes;
        drop(budget);
        let message = Message {
            value: Some(value),
            bytes,
            budget: Arc::clone(&self.budget),
        };
        if tx.unbounded_send(Ok(message)).is_err() {
            sender.take();
        }
    }
}

#[cfg(test)]
mod tests;
