use std::sync::Arc;

use gpui::{Context, Image};
use nmt_agent::chat::{Item as SessionItem, ReplayTurn};
use nmt_agent::transcript::TextField;
use nmt_agent::transcript::conversation::ConversationImage;
use nmt_profiling::transcript::{Operation, Probe};

use crate::agent_tab::transcript::view::TranscriptView;

/// Test-side writers. Each one changes the conversation the way the
/// controller does in production and then takes the change through
/// `sync_content`, so these tests measure the path the pane uses.
impl TranscriptView {
    /// Drop the conversation and every piece of view state derived from it.
    pub(crate) fn clear(&mut self) {
        self.conversation.borrow_mut().clear();

        self.sync_content();
    }

    /// Append one turn of a restored conversation under its own turn id, with
    /// the accounting the provider persisted for it.
    pub(crate) fn append_replay(&mut self, turn: u64, replay: ReplayTurn, cx: &mut Context<Self>) {
        let _profile = Probe::start(Operation::Replay);

        self.conversation.borrow_mut().replay(turn, replay);

        self.sync_content();

        cx.notify();
    }

    /// Append one entry under `turn`, stamped now.
    pub(crate) fn push_stamped(&mut self, turn: u64, item: SessionItem) {
        let _profile = Probe::start(Operation::AppendEntry);

        self.conversation.borrow_mut().push(turn, item, Vec::new());

        self.sync_content();
    }

    /// Append an item with the images it carries, as the pane does through
    /// the controller. Submitting a user message returns to the live tail.
    pub(crate) fn push(
        &mut self,
        turn: u64,
        item: SessionItem,
        images: Vec<Arc<Image>>,
        cx: &mut Context<Self>,
    ) {
        if matches!(&item, SessionItem::UserMessage { .. }) {
            self.scroll_to_bottom();
        }

        let images = images
            .into_iter()
            .map(|image| Arc::new(ConversationImage::new(image.bytes().into())))
            .collect();

        self.conversation.borrow_mut().push(turn, item, images);

        self.sync_content();

        cx.notify();
    }

    pub(crate) fn contains_item(&self, id: &str) -> bool {
        self.conversation.borrow().content.contains_item(id)
    }

    /// Fold an authoritative completed payload into the entry that streamed it.
    pub(crate) fn merge_completed(&mut self, item: &SessionItem) {
        let _profile = Probe::start(Operation::MergeCompleted);

        self.conversation.borrow_mut().merge_completed(item);

        self.sync_content();
    }

    /// Extend a streamed item's text. Returns whether the result is non-empty,
    /// which is what tells the caller the row became visible.
    pub(crate) fn append_delta(&mut self, item_id: &str, delta: &str, field: TextField) -> bool {
        let _profile = Probe::start(Operation::AppendDelta);

        let update = self
            .conversation
            .borrow_mut()
            .append_delta(item_id, delta, field);

        self.sync_content();

        update.is_some_and(|update| update.non_blank)
    }

    pub(crate) fn set_compacting(&mut self, compacting: bool, cx: &mut Context<Self>) {
        {
            let mut conversation = self.conversation.borrow_mut();

            let live_row = conversation.content.entries().len();

            conversation.live.set_compacting(compacting);

            conversation.changed(live_row, None);
        }

        self.sync_content();

        cx.notify();
    }

    pub(crate) fn was_interrupted(&self, turn: u64) -> bool {
        self.conversation.borrow().turns.was_interrupted(turn)
    }

    pub(crate) fn mark_interrupted(&mut self, turn: u64) {
        {
            let mut conversation = self.conversation.borrow_mut();

            conversation.turns.mark_interrupted(turn);

            conversation.changed_turn(turn);
        }

        self.sync_content();
    }

    /// Open the live turn the way the controller does when a prompt starts.
    pub(crate) fn start_working(&mut self, cx: &mut Context<Self>) {
        self.conversation.borrow_mut().start();

        self.sync_content();

        cx.notify();
    }

    /// Drop a turn that produced nothing, as the controller does on an
    /// immediate interrupt.
    pub(crate) fn discard_turn(&mut self, turn: u64, cx: &mut Context<Self>) {
        {
            let mut conversation = self.conversation.borrow_mut();

            conversation.live.discard();

            conversation.turns.forget(turn);

            conversation.changed_turn(turn);
        }

        self.sync_content();

        cx.notify();
    }

    /// Settle the running turn's duration and output usage for its status row.
    pub(crate) fn settle_turn(&mut self, turn: u64, cx: &mut Context<Self>) {
        self.conversation.borrow_mut().settle(turn);

        self.sync_content();

        cx.notify();
    }
}
