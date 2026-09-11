use std::time::Duration;

use futures::channel::mpsc;
use gpui::Context;
use nmt_agent::chat::Event;
use nmt_agent::message_memory::OUTPUT_FAILURE_METHOD;
use parking_lot::Mutex;
use serde_json::Value;

use crate::AgentPane;

impl AgentPane {
    pub(super) fn stop_for_output_failure(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(mut backend) = self.session.runtime.retire() {
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

pub(super) struct Message {
    value: Option<Value>,
}

impl Message {
    pub(super) fn take(&mut self) -> Value {
        self.value.take().expect("queued message is consumed once")
    }
}

pub(super) struct Sender {
    sender: Mutex<Option<mpsc::UnboundedSender<Result<Message, String>>>>,
}

pub(super) fn channel() -> (Sender, mpsc::UnboundedReceiver<Result<Message, String>>) {
    let (sender, receiver) = mpsc::unbounded();

    (
        Sender {
            sender: Mutex::new(Some(sender)),
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
        if value["method"] == OUTPUT_FAILURE_METHOD {
            let error = value["params"]["message"]
                .as_str()
                .unwrap_or("Agent output failed")
                .to_string();

            let _ = tx.unbounded_send(Err(error));

            sender.take();
            return;
        }

        let message = Message { value: Some(value) };

        if tx.unbounded_send(Ok(message)).is_err() {
            sender.take();
        }
    }
}

#[cfg(test)]
mod tests;
