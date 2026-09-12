//! Owned conversation entries and their indexed content updates, independent of
//! rendering, scroll position, and the provider that produced the messages.

pub mod conversation;
pub mod turns;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::mem;

use smallvec::SmallVec;

use crate::chat::Item;

/// One conversation entry. The caller supplies metadata such as a local display
/// stamp or attachment handles; content updates leave it untouched.
#[derive(Debug)]
pub struct TranscriptEntry<M = ()> {
    pub turn: u64,
    pub item: Item,
    pub metadata: M,
}

/// The provider field addressed by a streamed text update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextField {
    Reply,
    ReasoningSummary,
    CommandOutput,
}

/// The entry affected by an accepted delta. `previous_bytes` ends on a UTF-8
/// boundary, so callers can inspect the old prefix in the stored item without
/// copying it or counting characters on every update.
#[derive(Debug)]
pub struct TextAppend {
    pub index: usize,
    pub previous_bytes: usize,
    pub non_blank: bool,
}

/// One content owner for a conversation. Entries are readable by borrowing;
/// mutations also maintain the message index and return affected positions.
///
/// Metadata is stored inline with each entry instead of in a second collection.
/// This keeps it aligned with content without another allocation or lookup and
/// does not require metadata or conversation contents to be cloneable.
pub struct TranscriptContent<M = ()> {
    entries: Vec<TranscriptEntry<M>>,
    item_index: HashMap<String, SmallVec<[usize; 1]>>,
}

impl<M> Default for TranscriptContent<M> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            item_index: HashMap::new(),
        }
    }
}

impl<M> TranscriptContent<M> {
    // Row generation repeatedly borrows the same entries in Debug builds too;
    // the read-only interface must not add a call at each indexed access.
    #[inline(always)]
    pub fn entries(&self) -> &[TranscriptEntry<M>] {
        self.entries.as_slice()
    }

    /// Append live or restored content, preserving duplicate IDs in arrival
    /// order. Different item kinds may share an ID; updates select the first
    /// compatible entry rather than assuming IDs are unique across kinds.
    pub fn append(&mut self, entry: TranscriptEntry<M>) -> usize {
        let index = self.entries.len();

        if let Some(id) = entry.item.id() {
            self.item_index
                .entry(id.to_owned())
                .or_default()
                .push(index);
        }

        self.entries.push(entry);

        index
    }

    /// Replace a mirrored conversation and rebuild its index. Revision checks
    /// belong to the caller that owns the source; the supplied entries are moved.
    #[inline]
    pub fn replace(&mut self, entries: Vec<TranscriptEntry<M>>) {
        self.entries = entries;
        self.item_index.clear();

        for (index, entry) in self.entries.iter().enumerate() {
            if let Some(id) = entry.item.id() {
                self.item_index
                    .entry(id.to_owned())
                    .or_default()
                    .push(index);
            }
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.item_index.clear();
    }

    pub fn contains_item(&self, id: &str) -> bool {
        self.item_index.contains_key(id)
    }

    pub fn last_metadata_mut(&mut self) -> Option<&mut M> {
        self.entries.last_mut().map(|entry| &mut entry.metadata)
    }

    /// Return the first compatible entry that accepted the completed payload.
    /// Omitted fields retain their streamed values through Item::merge_completed.
    pub fn merge_completed(&mut self, item: &Item) -> Option<usize> {
        let positions = self.item_index.get(item.id()?)?;

        positions
            .iter()
            .copied()
            .find(|&index| self.entries[index].item.merge_completed(item))
    }

    /// Append in place without copying the transcript or creating an update list.
    /// Missing IDs and incompatible fields leave the conversation untouched.
    // Debug updates must not pay for an extra call and result transfer per chunk.
    // Optimized builds retain the compiler's normal inlining decisions.
    #[cfg_attr(debug_assertions, inline(always))]
    pub fn append_delta(
        &mut self,
        item_id: &str,
        delta: &str,
        field: TextField,
    ) -> Option<TextAppend> {
        let positions = self.item_index.get(item_id)?;

        for &index in positions {
            let text = match (field, &mut self.entries[index].item) {
                (TextField::Reply, Item::AgentMessage { text, .. }) => text,
                (TextField::ReasoningSummary, Item::Reasoning { summary, .. }) => summary,

                (
                    TextField::CommandOutput,
                    Item::CommandExecution {
                        aggregated_output, ..
                    },
                ) => aggregated_output,

                _ => continue,
            };

            let text = text.get_or_insert_default();
            let previous_bytes = text.len();

            text.push_str(delta);

            return Some(TextAppend {
                index,
                previous_bytes,
                non_blank: !text.trim().is_empty(),
            });
        }

        None
    }

    /// Latest non-blank assistant reply in a turn, for summaries or notifications.
    pub fn latest_agent_message(&self, turn: u64) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .find_map(|entry| match &entry.item {
                Item::AgentMessage {
                    text: Some(text), ..
                } if entry.turn == turn && !text.trim().is_empty() => Some(text.as_str()),

                _ => None,
            })
    }

    /// Providers may repeat an earlier error when they report turn completion.
    pub fn turn_has_error(&self, turn: u64, text: &str) -> bool {
        self.entries.iter().any(|entry| {
            entry.turn == turn
                && matches!(&entry.item, Item::Error { text: shown } if shown == text)
        })
    }

    pub fn turn_steps(&self, turn: u64) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.turn == turn && is_work_item(&entry.item))
            .count()
    }

    pub fn retain_last(&mut self, count: usize) -> usize {
        let dropped = self.entries.len().saturating_sub(count);

        if dropped > 0 {
            self.entries.drain(..dropped);

            let entries = mem::take(&mut self.entries);

            self.replace(entries);
        }

        dropped
    }

    /// Task lists are full replacements, so only the newest matching item counts.
    pub fn task_tally(&self) -> Option<(u32, u32)> {
        self.entries
            .iter()
            .rev()
            .find_map(|entry| entry.item.task_tally())
    }
}

/// Tool calls, file changes, and reasoning are work steps; conversation text and
/// context compactions describe the conversation rather than another action.
pub fn is_work_item(item: &Item) -> bool {
    matches!(
        item,
        Item::CommandExecution { .. }
            | Item::FileChange { .. }
            | Item::Other { .. }
            | Item::Reasoning { .. }
    )
}
