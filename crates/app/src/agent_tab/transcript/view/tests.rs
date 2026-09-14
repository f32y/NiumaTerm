use std::time::Instant;

use chrono::Utc;
use gpui::Context;
use nmt_agent::chat::{Item as SessionItem, ReplayTurn};
use nmt_agent::transcript::TextField;
use nmt_profiling::transcript::{Operation, Probe};

use crate::agent_tab::transcript::Entry;
use crate::agent_tab::transcript::rows::EntryPresentation;
use crate::agent_tab::transcript::view::{TranscriptView, Typewriter};

impl TranscriptView {
    /// Mirror a conversation this view does not own. `revision` identifies the
    /// source's current content, so an unchanged source costs one comparison
    /// rather than a rebuild. Row indices stay stable because the source only
    /// appends and merges in place, which keeps expansion state valid.
    pub fn show_items(&mut self, items: &[SessionItem], revision: u64, cx: &mut Context<Self>) {
        if self.source_revision == Some(revision) {
            return;
        }

        let _profile = Probe::start(Operation::MirrorRebuild);

        self.source_revision = Some(revision);

        self.conversation.borrow_mut().content.replace(
            items
                .iter()
                .map(|item| Entry {
                    turn: 0,
                    item: item.clone(),
                    metadata: EntryPresentation::default(),
                })
                .collect(),
        );

        self.code_transcripts.invalidate_from(0);

        self.row_cache.invalidate(0);

        cx.notify();
    }

    /// Drop the conversation and every piece of view state derived from it.
    /// Used when the owning view switches to a different conversation, so one
    /// conversation's expansion and scroll position cannot leak into another's.
    pub(crate) fn clear(&mut self) {
        self.conversation.borrow_mut().clear();
        self.reset_presentation();
    }

    /// Append one turn of a restored conversation under its own turn id, with
    /// the accounting the provider persisted for it. Replaying it settles the
    /// turn, so it folds its work exactly like one completed in this process.
    /// Its duration is a separate question: the transcript file records none,
    /// so a replayed turn usually closes without an elapsed-time line rather
    /// than stating a time the session never reported.
    pub(crate) fn append_replay(&mut self, turn: u64, replay: ReplayTurn, cx: &mut Context<Self>) {
        let _profile = Probe::start(Operation::Replay);

        if replay.items.is_empty() {
            self.invalidate_turn_rows(turn);
        }

        for entry in replay.items {
            self.append_entry(Entry {
                turn,
                item: entry.item,
                metadata: EntryPresentation {
                    at: entry.at,
                    images: Vec::new(),
                },
            });
        }

        self.conversation.borrow_mut().turns.replay(
            turn,
            replay.interrupted,
            replay.seconds,
            replay.output_tokens,
        );

        cx.notify();
    }

    /// Append one entry with an explicit stamp, for content this view records
    /// outside the normal push path.
    pub(crate) fn push_stamped(&mut self, turn: u64, item: SessionItem) {
        self.append_entry(Entry {
            turn,
            item,
            metadata: EntryPresentation {
                at: Some(Utc::now().timestamp()),
                images: Vec::new(),
            },
        });
    }

    pub(crate) fn contains_item(&self, id: &str) -> bool {
        self.conversation.borrow().content.contains_item(id)
    }

    /// Fold an authoritative completed payload into the entry that streamed it.
    pub(crate) fn merge_completed(&mut self, item: &SessionItem) {
        let _profile = Probe::start(Operation::MergeCompleted);

        if let Some(index) = self.conversation.borrow_mut().content.merge_completed(item) {
            self.code_transcripts.invalidate(index);
            self.row_cache.invalidate(index);
        }
    }

    /// Extend a streamed item's text. Returns whether the result is non-empty,
    /// which is what tells the caller the row became visible.
    pub(crate) fn append_delta(&mut self, item_id: &str, delta: &str, field: TextField) -> bool {
        let _profile = Probe::start(Operation::AppendDelta);

        let Some(update) = self
            .conversation
            .borrow_mut()
            .append_delta(item_id, delta, field)
        else {
            return false;
        };

        let index = update.index;

        // Only a newly selected reply needs its old prefix counted. Existing
        // typed edges keep advancing in the view without rescanning each delta.
        if matches!(field, TextField::Reply)
            && !self
                .typewriter
                .as_ref()
                .is_some_and(|typing| typing.index() == index)
        {
            if let Some(previous) = &self.typewriter {
                self.row_cache.invalidate(previous.index());
            }

            if let SessionItem::AgentMessage {
                text: Some(text), ..
            } = &self.conversation.borrow().content.entries()[index].item
            {
                self.typewriter = Some(Typewriter::start(
                    index,
                    text[..update.previous_bytes].chars().count(),
                    Instant::now(),
                ));
            }
        }

        self.code_transcripts.invalidate(index);
        self.row_cache.invalidate(index);

        update.non_blank
    }

    pub(crate) fn set_compacting(&mut self, compacting: bool, cx: &mut Context<Self>) {
        self.conversation
            .borrow_mut()
            .live
            .set_compacting(compacting);

        self.row_cache
            .invalidate(self.conversation.borrow().content.entries().len());

        cx.notify();
    }

    pub(crate) fn was_interrupted(&self, turn: u64) -> bool {
        self.conversation.borrow().turns.was_interrupted(turn)
    }

    pub(crate) fn mark_interrupted(&mut self, turn: u64) {
        self.conversation.borrow_mut().turns.mark_interrupted(turn);
        self.invalidate_turn_rows(turn);
    }

    /// Settle the running turn's duration and output usage for its status row.
    /// These are view state rather than provider transcript content, so they
    /// stay outside the shared item stream.
    pub(crate) fn settle_turn(&mut self, turn: u64, cx: &mut Context<Self>) {
        let Some((started, output_tokens)) = self.conversation.borrow_mut().live.finish() else {
            return;
        };

        self.conversation.borrow_mut().turns.settle(
            turn,
            started.elapsed().as_secs(),
            output_tokens,
        );

        self.invalidate_turn_rows(turn);

        cx.notify();
    }
}
