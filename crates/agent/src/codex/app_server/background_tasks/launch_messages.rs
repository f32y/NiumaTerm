use std::collections::HashMap;

use indexmap::IndexMap;
use serde_json::Value;

use crate::chat::Item;

const MAX_PENDING_MESSAGES: usize = 64;

#[derive(Default)]
pub(super) struct LaunchMessages {
    confirmed: HashMap<String, String>,

    /// Messages for children not yet proven to belong here, oldest first.
    pending: IndexMap<String, String>,
}

impl LaunchMessages {
    pub(super) fn clear(&mut self) {
        self.confirmed.clear();

        self.pending.clear();
    }

    pub(super) fn confirm(&mut self, thread_id: &str) {
        let Some(message) = self.pending.shift_remove(thread_id) else {
            return;
        };

        self.confirmed
            .entry(thread_id.to_owned())
            .or_insert(message);
    }

    pub(super) fn remember(&mut self, thread_id: &str, message: &str) {
        if message.trim().is_empty() {
            return;
        }

        self.confirmed
            .entry(thread_id.to_owned())
            .or_insert_with(|| message.to_owned());
    }

    /// Save the first inter-agent message emitted for a child. Only plaintext
    /// input blocks are useful to the transcript UI; encrypted content remains
    /// opaque and is left out.
    pub(super) fn observe(&mut self, thread_id: &str, item: &Value, confirmed: bool) -> bool {
        if item["type"].as_str() != Some("agent_message") {
            return false;
        }

        let Some(content) = item["content"].as_array().filter(|content| {
            content
                .iter()
                .all(|block| block["type"].as_str() == Some("input_text"))
        }) else {
            return false;
        };

        let message = content
            .iter()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");

        if message.trim().is_empty() {
            return false;
        }

        if confirmed {
            if self.confirmed.contains_key(thread_id) {
                return false;
            }

            self.confirmed.insert(thread_id.to_owned(), message);

            return true;
        }

        if self.pending.contains_key(thread_id) {
            return false;
        }

        if self.pending.len() >= MAX_PENDING_MESSAGES {
            self.pending.shift_remove_index(0);
        }

        self.pending.insert(thread_id.to_owned(), message);

        true
    }

    /// Add a known launch instruction when stored thread history does not
    /// already include it.
    pub(super) fn prepend(&self, thread_id: &str, mut items: Vec<Item>) -> Vec<Item> {
        let Some(message) = self.confirmed.get(thread_id) else {
            return items;
        };

        let included = items.iter().any(|item| {
            matches!(item, Item::UserMessage { text: Some(text) } if text.trim() == message.trim())
        });

        if !included {
            items.insert(
                0,
                Item::UserMessage {
                    text: Some(message.clone()),
                },
            );
        }

        items
    }
}
