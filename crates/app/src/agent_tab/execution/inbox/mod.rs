use std::time::Duration;

use futures::channel::mpsc;
use nmt_agent::chat::Event;
use nmt_agent::message_memory::OUTPUT_FAILURE_METHOD;
use parking_lot::Mutex;
use serde_json::Value;

pub(in crate::agent_tab) struct Message {
    value: Option<Value>,
}

impl Message {
    pub(in crate::agent_tab) fn take(&mut self) -> Value {
        self.value.take().expect("queued message is consumed once")
    }
}

pub(in crate::agent_tab) struct Sender {
    sender: Mutex<Option<mpsc::UnboundedSender<Result<Message, String>>>>,
}

pub(in crate::agent_tab) fn channel() -> (Sender, mpsc::UnboundedReceiver<Result<Message, String>>)
{
    let (sender, receiver) = mpsc::unbounded();

    (
        Sender {
            sender: Mutex::new(Some(sender)),
        },
        receiver,
    )
}

impl Sender {
    pub(in crate::agent_tab) fn send(&self, value: Value) {
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
mod inbox_tests;

pub(in crate::agent_tab) const MAX_MESSAGES_PER_BATCH: usize = 64;
pub(in crate::agent_tab) const MAX_UPDATE_TIME: Duration = Duration::from_millis(2);

/// Only adjacent deltas can share an update. Every other event is applied
/// immediately, so approvals and turn transitions retain their side effects
/// before the next backend message is processed.
#[derive(Default)]
pub(in crate::agent_tab) struct EventBatch {
    pending: Option<Event>,
}

impl EventBatch {
    pub(in crate::agent_tab) fn push(&mut self, event: Event, mut apply: impl FnMut(Event)) {
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

    pub(in crate::agent_tab) fn flush(&mut self, mut apply: impl FnMut(Event)) {
        if let Some(event) = self.pending.take() {
            apply(event);
        }
    }
}

#[cfg(test)]
mod output_tests;
