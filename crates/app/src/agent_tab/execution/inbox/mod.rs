#[cfg(test)]
mod inbox_tests;

#[cfg(test)]
mod output_tests;

use std::time::Duration;

use futures::channel::mpsc;
use nmt_agent::chat::Event;
use nmt_agent::message_memory::OUTPUT_FAILURE_METHOD;
use parking_lot::Mutex;
use serde_json::Value;

pub(crate) struct Sender {
    sender: Mutex<Option<mpsc::UnboundedSender<Result<Value, String>>>>,
}

pub(crate) fn channel() -> (Sender, mpsc::UnboundedReceiver<Result<Value, String>>) {
    let (sender, receiver) = mpsc::unbounded();

    (
        Sender {
            sender: Mutex::new(Some(sender)),
        },
        receiver,
    )
}

impl Sender {
    pub(crate) fn send(&self, value: Value) {
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

        if tx.unbounded_send(Ok(value)).is_err() {
            sender.take();
        }
    }
}

pub(crate) const MAX_MESSAGES_PER_BATCH: usize = 64;
pub(crate) const MAX_UPDATE_TIME: Duration = Duration::from_millis(2);

/// Only adjacent deltas can share an update. Every other event is applied
/// immediately, so approvals and turn transitions retain their side effects
/// before the next backend message is processed.
#[derive(Default)]
pub(crate) struct EventBatch {
    pending: Option<Event>,
}

impl EventBatch {
    pub(crate) fn push(&mut self, event: Event, mut apply: impl FnMut(Event)) {
        match (&mut self.pending, event) {
            (
                Some(Event::AgentMessageDelta { item_id, delta }),
                Event::AgentMessageDelta {
                    item_id: next_id,
                    delta: next,
                },
            )
            | (
                Some(Event::ReasoningSummaryDelta { item_id, delta }),
                Event::ReasoningSummaryDelta {
                    item_id: next_id,
                    delta: next,
                },
            )
            | (
                Some(Event::CommandOutputDelta { item_id, delta }),
                Event::CommandOutputDelta {
                    item_id: next_id,
                    delta: next,
                },
            ) if *item_id == next_id => delta.push_str(&next),
            (_, event) => {
                self.flush(&mut apply);

                match event {
                    Event::AgentMessageDelta { .. }
                    | Event::ReasoningSummaryDelta { .. }
                    | Event::CommandOutputDelta { .. } => self.pending = Some(event),
                    event => apply(event),
                }
            }
        }
    }

    pub(crate) fn flush(&mut self, mut apply: impl FnMut(Event)) {
        if let Some(event) = self.pending.take() {
            apply(event);
        }
    }
}
