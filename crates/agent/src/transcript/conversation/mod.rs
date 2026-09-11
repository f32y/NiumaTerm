//! Retained conversation data, independent of any attached renderer.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use uuid::Uuid;

use crate::chat::{ContextComposition, ContextWindowUsage, Item, ReplayTurn, SessionStats};
use crate::transcript::turns::{LiveTurn, TurnLedger};
use crate::transcript::{TextAppend, TextField, TranscriptContent, TranscriptEntry};

/// Immutable PNG data retained after an image submission is accepted.
#[derive(Debug)]
pub struct ConversationImage {
    pub id: Uuid,
    pub bytes: Arc<[u8]>,
}

impl ConversationImage {
    pub fn new(bytes: Arc<[u8]>) -> Self {
        Self {
            id: Uuid::new_v4(),
            bytes,
        }
    }
}

/// Semantic entry data. Rendering formats timestamps and caches image previews.
#[derive(Default, Debug)]
pub struct EntryMetadata {
    pub at: Option<i64>,
    pub images: Vec<Arc<ConversationImage>>,
}

/// The entries and observed accounting for one conversation.
#[derive(Default)]
pub struct ConversationState {
    pub content: TranscriptContent<EntryMetadata>,
    pub turns: TurnLedger,
    pub live: LiveTurn,
    pub context_window_usage: Option<ContextWindowUsage>,
    pub context_composition: Option<ContextComposition>,
    pub session_stats: Option<SessionStats>,
    pub submitted_at: Option<Instant>,
    pub first_output_latency: Option<Duration>,
    pub last_response_at: Option<Instant>,
    generation: u64,
    revision: u64,
    changes: VecDeque<ContentChange>,
}

#[derive(Clone, Copy, Debug)]
pub struct ContentChange {
    pub revision: u64,
    pub first: usize,
    pub reply: Option<(usize, usize)>,
}

impl ConversationState {
    pub fn attach_last_images(&mut self, images: Vec<Arc<ConversationImage>>) {
        if let Some(metadata) = self.content.last_metadata_mut() {
            metadata.images = images;
            self.changed(self.content.entries().len().saturating_sub(1), None);
        }
    }

    pub fn version(&self) -> (u64, u64) {
        (self.generation, self.revision)
    }

    /// Retain a bounded window of invalidations, falling back to current content
    /// when a reader missed more updates than the window holds.
    pub fn changes_since(&self, version: (u64, u64)) -> Option<ContentChange> {
        if version == self.version() {
            return None;
        }

        if version.0 != self.generation
            || self
                .changes
                .front()
                .is_none_or(|change| version.1.saturating_add(1) < change.revision)
        {
            return Some(ContentChange {
                revision: self.revision,
                first: 0,
                reply: None,
            });
        }

        let mut change = ContentChange {
            revision: self.revision,
            first: self.content.entries().len(),
            reply: None,
        };

        for next in self
            .changes
            .iter()
            .filter(|change| change.revision > version.1)
        {
            change.first = change.first.min(next.first);

            if next.reply.is_some() {
                change.reply = next.reply;
            }
        }

        Some(change)
    }

    pub fn changed(&mut self, first: usize, reply: Option<(usize, usize)>) {
        self.revision += 1;

        if self.changes.len() == 64 {
            self.changes.pop_front();
        }

        self.changes.push_back(ContentChange {
            revision: self.revision,
            first,
            reply,
        });
    }

    pub fn append(&mut self, entry: TranscriptEntry<EntryMetadata>) -> usize {
        let index = self.content.append(entry);

        self.changed(index.saturating_sub(1), None);

        index
    }

    pub fn push(&mut self, turn: u64, item: Item, images: Vec<Arc<ConversationImage>>) {
        self.append(TranscriptEntry {
            turn,
            item,
            metadata: EntryMetadata {
                at: Some(Utc::now().timestamp()),
                images,
            },
        });
    }

    pub fn replay(&mut self, turn: u64, replay: ReplayTurn) {
        let first = self.content.entries().len();

        for entry in replay.items {
            self.content.append(TranscriptEntry {
                turn,
                item: entry.item,
                metadata: EntryMetadata {
                    at: entry.at,
                    images: Vec::new(),
                },
            });
        }

        self.turns.replay(
            turn,
            replay.interrupted,
            replay.seconds,
            replay.output_tokens,
        );

        self.changed(first.saturating_sub(1), None);
    }

    pub fn merge_completed(&mut self, item: &Item) {
        if let Some(index) = self.content.merge_completed(item) {
            self.changed(index, None);
        }
    }

    pub fn append_delta(
        &mut self,
        item_id: &str,
        delta: &str,
        field: TextField,
    ) -> Option<TextAppend> {
        let update = self.content.append_delta(item_id, delta, field)?;
        let reply = (field == TextField::Reply).then_some((update.index, update.previous_bytes));

        self.changed(update.index, reply);

        Some(update)
    }

    pub fn start(&mut self) {
        self.submitted_at = Some(Instant::now());
        self.live.start();
        self.changed(self.content.entries().len(), None);
    }

    pub fn visible_output(&mut self) {
        if let Some(at) = self.submitted_at.take() {
            self.first_output_latency = Some(at.elapsed());
        }
    }

    pub fn settle(&mut self, turn: u64) {
        self.live.set_compacting(false);

        if let Some((started, output_tokens)) = self.live.finish() {
            self.turns
                .settle(turn, started.elapsed().as_secs(), output_tokens);
        }

        self.last_response_at = Some(Instant::now());
        self.changed_turn(turn);
    }

    pub fn changed_turn(&mut self, turn: u64) {
        let first = self
            .content
            .entries()
            .iter()
            .position(|entry| entry.turn == turn)
            .unwrap_or(self.content.entries().len());

        self.changed(first, None);
    }

    pub fn retain_last(&mut self, count: usize) -> usize {
        let dropped = self.content.retain_last(count);

        if dropped > 0 {
            self.generation += 1;
            self.changes.clear();
            self.changed(0, None);
        }

        dropped
    }

    pub fn clear(&mut self) {
        self.content.clear();
        self.turns.clear();
        self.live.discard();
        self.context_window_usage = None;
        self.context_composition = None;
        self.session_stats = None;
        self.submitted_at = None;
        self.first_output_latency = None;
        self.last_response_at = None;
        self.generation += 1;
        self.revision = 0;
        self.changes.clear();
    }
}

pub fn hidden(item: &Item) -> bool {
    match item {
        Item::UserMessage { text }
        | Item::AgentMessage { text, .. }
        | Item::Reasoning { summary: text, .. } => {
            text.as_deref().is_none_or(|text| text.trim().is_empty())
        }

        _ => false,
    }
}

#[cfg(test)]
mod tests;
