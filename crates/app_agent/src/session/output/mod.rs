use std::time::Duration;

use nmt_agent_utils::chat::Event;

pub(super) const MAX_MESSAGES_PER_BATCH: usize = 64;
pub(super) const MAX_UPDATE_TIME: Duration = Duration::from_millis(2);

/// Only adjacent deltas can share an update. Every other event is applied
/// immediately, so approvals and turn transitions retain their side effects
/// before the next backend message is processed.
#[derive(Default)]
pub(super) struct EventBatch {
    pending: Option<Event>,
}

impl EventBatch {
    pub(super) fn push(&mut self, event: Event, mut apply: impl FnMut(Event)) {
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

    pub(super) fn flush(&mut self, mut apply: impl FnMut(Event)) {
        if let Some(event) = self.pending.take() {
            apply(event);
        }
    }
}

#[cfg(test)]
mod tests;
