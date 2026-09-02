//! Identifier aliases for one session.
//!
//! One child is named several ways over its life: by task id, by the tool-use
//! id of the call that launched it, and by an agent id. Only a record that
//! carried two of them together proves they describe the same child, so this
//! records exactly those pairings and nothing inferred from recency.

use std::collections::{HashMap, VecDeque};

/// Identifier aliases retained per session. One child contributes at most a
/// handful (task, tool-use, agent), so this only bounds a stream that keeps
/// inventing identifiers.
const MAX_ALIASES: usize = 512;

#[derive(Default)]
pub(super) struct AliasTable {
    aliases: HashMap<String, String>,
    order: VecDeque<String>,
}

impl AliasTable {
    pub(super) fn clear(&mut self) {
        self.aliases.clear();
        self.order.clear();
    }

    /// The canonical id this identifier was recorded against, if any.
    pub(super) fn lookup(&self, id: &str) -> Option<&str> {
        self.aliases.get(id).map(String::as_str)
    }

    /// Record that these identifiers describe the same child. Only called with
    /// identifiers a single record carried together.
    pub(super) fn link_all(&mut self, canonical: &str, ids: &[String]) {
        for id in ids {
            if id == canonical || self.aliases.contains_key(id) {
                continue;
            }
            if self.order.len() >= MAX_ALIASES
                && let Some(oldest) = self.order.pop_front()
            {
                self.aliases.remove(&oldest);
            }
            self.order.push_back(id.clone());
            self.aliases.insert(id.clone(), canonical.to_owned());
        }
    }
}
