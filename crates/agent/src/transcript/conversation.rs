//! Retained conversation data, independent of any attached renderer.

#[cfg(test)]
#[path = "conversation_tests.rs"]
mod conversation_tests;

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use smallvec::{SmallVec, smallvec};
use uuid::Uuid;

use crate::chat::{ContextComposition, ContextWindowUsage, Item, ReplayTurn, SessionStats};
use crate::transcript::turns::{GenerationSpeed, GenerationStats, LiveTurn, TurnLedger};
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
    pub generation_stats: GenerationStats,
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

#[derive(Clone, Debug)]
pub struct ContentChange {
    pub revision: u64,
    pub first: usize,
    pub reply: Option<(usize, usize)>,

    /// Text updates do not invalidate cached output for neighbouring entries.
    pub entries: SmallVec<[(usize, EntryChange); 1]>,

    /// Unknown mutations and expired history require reading current content.
    pub reset_from: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryChange {
    Append { previous_bytes: usize },
    Replace,
}

impl ConversationState {
    pub fn session_generation_speed(&self) -> Option<GenerationSpeed> {
        match self.session_stats {
            Some(stats) => GenerationSpeed::from_totals(
                stats.decode_tokens,
                Duration::from_millis(stats.decode_ms),
                false,
            ),
            None => self.generation_stats.session_speed(),
        }
    }

    pub(crate) fn attach_last_images(&mut self, images: Vec<Arc<ConversationImage>>) {
        if let Some(metadata) = self.content.last_metadata_mut() {
            metadata.images = images;

            self.presentation_changed(self.content.entries().len().saturating_sub(1));
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
                entries: SmallVec::new(),
                reset_from: Some(0),
            });
        }

        let mut change = ContentChange {
            revision: self.revision,
            first: self.content.entries().len(),
            reply: None,
            entries: SmallVec::new(),
            reset_from: None,
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

            if let Some(first) = next.reset_from {
                change.reset_from = Some(change.reset_from.map_or(first, |old| old.min(first)));
            }

            for &(index, update) in &next.entries {
                if let Some((_, previous)) = change.entries.iter_mut().find(|(i, _)| *i == index) {
                    *previous = match (*previous, update) {
                        (
                            EntryChange::Append {
                                previous_bytes: old,
                            },
                            EntryChange::Append {
                                previous_bytes: new,
                            },
                        ) => EntryChange::Append {
                            previous_bytes: old.min(new),
                        },
                        _ => EntryChange::Replace,
                    };
                } else {
                    change.entries.push((index, update));
                }
            }
        }

        Some(change)
    }

    pub fn changed(&mut self, first: usize, reply: Option<(usize, usize)>) {
        self.record_change(first, reply, SmallVec::new(), Some(first));
    }

    fn presentation_changed(&mut self, first: usize) {
        self.record_change(first, None, SmallVec::new(), None);
    }

    fn record_change(
        &mut self,
        first: usize,
        reply: Option<(usize, usize)>,
        entries: SmallVec<[(usize, EntryChange); 1]>,
        reset_from: Option<usize>,
    ) {
        self.revision += 1;

        if self.changes.len() == 64 {
            self.changes.pop_front();
        }

        self.changes.push_back(ContentChange {
            revision: self.revision,
            first,
            reply,
            entries,
            reset_from,
        });
    }

    pub fn append(&mut self, entry: TranscriptEntry<EntryMetadata>) -> usize {
        let index = self.content.append(entry);

        self.presentation_changed(index.saturating_sub(1));

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

        self.generation_stats.begin_turn();

        for sample in replay.generation_samples {
            self.generation_stats.record(sample);
        }

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

        self.presentation_changed(first.saturating_sub(1));
    }

    pub fn merge_completed(&mut self, item: &Item) {
        if let Some(index) = self.content.merge_completed(item) {
            match item {
                Item::CommandExecution {
                    aggregated_output: None,
                    ..
                }
                | Item::FileChange { diff: None, .. }
                | Item::Other { output: None, .. } => self.presentation_changed(index),
                _ => {
                    self.record_change(index, None, smallvec![(index, EntryChange::Replace)], None)
                }
            }
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

        self.record_change(
            update.index,
            reply,
            smallvec![(
                update.index,
                EntryChange::Append {
                    previous_bytes: update.previous_bytes
                }
            )],
            None,
        );

        Some(update)
    }

    pub fn start(&mut self) {
        self.submitted_at = Some(Instant::now());

        self.generation_stats.begin_turn();

        self.live.start();

        self.presentation_changed(self.content.entries().len());
    }

    pub(crate) fn visible_output(&mut self) {
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

        self.presentation_changed(first);
    }

    /// Replace every entry from `from` on, as a replica following another
    /// process's conversation does.
    pub fn splice(&mut self, from: usize, entries: Vec<TranscriptEntry<EntryMetadata>>) {
        let from = from.min(self.content.entries().len());

        self.content.truncate(from);

        for entry in entries {
            self.content.append(entry);
        }

        self.changed(from, None);
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

        self.generation_stats = GenerationStats::default();

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
