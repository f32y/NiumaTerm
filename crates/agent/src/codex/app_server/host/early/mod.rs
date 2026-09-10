//! Retain early traffic until its owning tab can receive it.

use std::collections::HashMap;

use serde_json::Value;

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(super) struct EarlyMessages {
    threads: HashMap<String, Vec<Value>>,
}

impl EarlyMessages {
    pub(super) fn hold(&mut self, thread_id: &str, value: Value) {
        // Ownership can arrive after arbitrary amounts of activity. Eviction
        // would lose turn state or an approval the server is waiting on.
        self.threads
            .entry(thread_id.to_owned())
            .or_default()
            .push(value);
    }

    pub(super) fn take(&mut self, thread_id: &str) -> Vec<Value> {
        self.threads.remove(thread_id).unwrap_or_default()
    }

    pub(super) fn forget(&mut self, thread_id: &str) {
        self.threads.remove(thread_id);
    }

    pub(super) fn clear(&mut self) {
        self.threads.clear();
    }
}
