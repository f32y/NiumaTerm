use std::collections::HashMap;

use serde_json::Value;

use crate::codex::app_server::FIRST_TURN_RPC_ID;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum QueryKind {
    Start,
    Models,
    History,
    Resume,
    Checkpoints,
    Fork,
}

impl QueryKind {
    fn replaces(self, previous: Self) -> bool {
        self == previous
            || (matches!(self, Self::Start | Self::Resume | Self::Fork)
                && matches!(previous, Self::Start | Self::Resume | Self::Fork))
    }
}

pub(super) enum ControlOperation {
    Query(QueryKind),
    Other,
    ThreadRequest,
    Command(String),
    ThreadName,
}

/// Dynamic requests are consumed once. Clearing a thread's operations makes
/// late replies harmless without retaining a growing set of cancelled ids.
pub(super) struct ControlState {
    next_id: u64,
    pending: HashMap<u64, ControlOperation>,
    closed: bool,
}

impl Default for ControlState {
    fn default() -> Self {
        Self {
            next_id: FIRST_TURN_RPC_ID,
            pending: HashMap::new(),
            closed: false,
        }
    }
}

impl ControlState {
    pub(super) fn request_ids(&self) -> Vec<u64> {
        self.pending.keys().copied().collect()
    }

    pub(super) fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub(super) fn outgoing(message: &Value) -> Option<(u64, ControlOperation)> {
        let method = message["method"].as_str()?;
        let id = message["id"]
            .as_u64()
            .filter(|id| *id >= FIRST_TURN_RPC_ID)?;

        let operation = match method {
            "thread/name/set" => ControlOperation::ThreadName,
            _ if message["params"]["threadId"].is_string() => ControlOperation::ThreadRequest,
            _ => ControlOperation::Other,
        };

        Some((id, operation))
    }

    pub(super) fn track(&mut self, id: u64, operation: ControlOperation) {
        if !self.closed {
            self.pending.insert(id, operation);
        }
    }

    pub(super) fn finish(&mut self, id: u64) -> Option<ControlOperation> {
        self.pending.remove(&id)
    }

    pub(super) fn track_query(&mut self, id: u64, kind: QueryKind) {
        // A unique id is also the generation token: removing the previous
        // request rejects both its late success and its late failure.
        self.pending.retain(|_, operation| !matches!(operation, ControlOperation::Query(previous) if kind.replaces(*previous)));
        self.track(id, ControlOperation::Query(kind));
    }

    pub(super) fn has_command(&self) -> bool {
        self.pending
            .values()
            .any(|operation| matches!(operation, ControlOperation::Command(_)))
    }

    pub(super) fn reset_thread(&mut self) {
        // Catalog and descendant requests have separate owners that still
        // need their responses to release their own in-flight state.
        self.pending.retain(|_, operation| {
            matches!(
                operation,
                ControlOperation::Other
                    | ControlOperation::Query(QueryKind::Models | QueryKind::History)
            )
        });
    }

    pub(super) fn close(&mut self) {
        self.pending.clear();
        self.closed = true;
    }

    pub(super) fn is_closed(&self) -> bool {
        self.closed
    }

    #[cfg(test)]
    pub(super) fn next_id(&self) -> u64 {
        self.next_id
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}
