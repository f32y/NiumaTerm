#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::HashMap;
use std::mem;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use gpui::prelude::*;
use gpui::{
    AnyElement, Bounds, Context, FollowMode, Image, ImageFormat, IntoElement, ListAlignment,
    ListOffset, ListState, Pixels, Render, ScrollHandle, SharedString, Window, div, list, px,
    relative,
};
use gpui_component::button::Button;
use gpui_component::modern_menu::ModernMenuExt as _;
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme as _, ElementExt as _, IconName, Sizable as _};
use nmt_agent::chat::Item as SessionItem;
use nmt_agent::transcript::conversation::{ConversationImage, ConversationState};
use nmt_config::agent::CollapseRows;
use nmt_profiling::transcript::{Operation, Probe};
use rust_i18n::t;

use crate::agent_tab::AgentPane;
use crate::agent_tab::capabilities::AgentCapabilities as _;
use crate::agent_tab::composer::{PALETTE_MAX_HEIGHT, PromptTarget};
use crate::agent_tab::profile::AgentKind;
use crate::agent_tab::settings::{AgentSettings, UI_RADIUS};
use crate::agent_tab::transcript::code::is_code_item;
use crate::agent_tab::transcript::incremental::RowCache;
use crate::agent_tab::transcript::render::image_preview::{ImagePreview, ImagePreviewLayer};
use crate::agent_tab::transcript::render::menus::{copy_entry_menu, prompt_row_menu};
use crate::agent_tab::transcript::render::message_rows::{
    agent_reply_row, error_row, question_message, working_row,
};
use crate::agent_tab::transcript::render::text_style::{markdown_view, transcript_text_style};
use crate::agent_tab::transcript::render::user_row::user_prompt_row;
use crate::agent_tab::transcript::render::work_card::{work_card, work_step};
use crate::agent_tab::transcript::render::{
    TRANSCRIPT_LINE_HEIGHT, TRANSCRIPT_RUN_RULE, TRANSCRIPT_TEXT_INSET, bounded_scroll,
    compaction_row, gap_px, render_interrupted_row, render_run_toggle, render_turn_fold,
    render_turn_summary, transcript_column,
};
use crate::agent_tab::transcript::reveal::{Disclosures, RevealKey, revealed_block, revealed_part};
use crate::agent_tab::transcript::row_structure::{RowGeometry, RowSource};
use crate::agent_tab::transcript::rows::{
    EntryPresentation, PickerReservation, RowGap, TranscriptRow, folds_turns, is_run_row,
    spaced_rows, turn_opening_prompts,
};
use crate::agent_tab::transcript::typewriter::ReplyTyping;
use crate::agent_tab::transcript::{
    CodeTranscriptCache, Entry, RowSpec, is_work_row, should_show_jump_to_latest,
};

/// One agent conversation as the user reads it: the entry list, the row
/// structure derived from it, and every piece of view state that structure
/// depends on.
///
/// This is an entity rather than a set of helpers on the owning view because
/// its disclosure rows toggle expansion state from click handlers, and because
/// each conversation needs its own [`ListState`] — that state caches measured
/// row heights, so two conversations cannot share one. Both the Agent pane's
/// own conversation and a child agent's conversation render through here, which
/// is what keeps their presentation from drifting apart.
pub struct TranscriptView {
    pub(crate) conversation: Rc<RefCell<ConversationState>>,
    pub(crate) image_previews: RefCell<HashMap<(usize, usize), Arc<Image>>>,
    pub(super) row_cache: RowCache,

    /// Virtualized transcript: only visible rows build elements each frame.
    /// `rows` mirrors the list's item count; render() rebuilds the changed
    /// turn suffix and splices/remeasures just the changed range.
    pub(crate) transcript_list: ListState,

    pub(crate) rows: Vec<TranscriptRow>,

    pub(super) preview: ImagePreviewLayer,
    pub(super) picker: PickerReservation,

    /// Row heights depend on the prose and technical-content fonts, which the
    /// specs can't see; the last-seen values trigger a full remeasure on change.
    transcript_font: (SharedString, f32, SharedString, f32),

    /// Collapse setting the rows above were built under, so a change to it can
    /// retire the per-turn and per-run departures from the mode it replaces.
    collapse_mode: CollapseRows,

    /// Which parts of the transcript are open, how far through their motion
    /// they are, and how tall each one lays out to.
    pub(crate) disclosures: Disclosures,

    /// Expanded technical output retains its parsed source and scroll position.
    /// Collapsing a row releases the extra source, syntax trees, and worker.
    pub(crate) code_transcripts: CodeTranscriptCache,

    /// The reply being let onto the screen a character at a time.
    typing: ReplyTyping,

    /// Presentation inputs rather than owned state: the working directory
    /// resolves transcript links, and the provider decides a few labels.
    pub(crate) cwd: Option<String>,

    pub(crate) kind: AgentKind,

    /// Revision of the conversation this view was last filled from, for a view
    /// that mirrors content someone else owns rather than accumulating its own.
    source_revision: Option<u64>,

    observed_version: (u64, u64),

    /// The pane whose conversation this is, for the row actions that address
    /// the conversation rather than the row: branching in front of a prompt,
    /// rewinding to one. Absent on a view that mirrors somebody else's
    /// conversation — a child agent's or a workflow member's — where those
    /// actions have no conversation of this pane's to act on.
    owner: Option<gpui::WeakEntity<AgentPane>>,

    pub(crate) attribution: HashMap<String, TranscriptAttribution>,
}

pub(crate) struct TranscriptAttribution {
    pub(crate) name: SharedString,
    pub(crate) cwd: Option<String>,
}

impl TranscriptView {
    pub fn new(kind: AgentKind, cwd: Option<String>) -> Self {
        let collapse_mode = CollapseRows::default();

        Self {
            conversation: Rc::new(RefCell::new(ConversationState::default())),
            image_previews: Default::default(),
            row_cache: RowCache::default(),
            transcript_list: {
                // Bottom alignment + tail follow give chat-log behavior: pinned
                // to the newest row until the user scrolls up, re-engaging when
                // they return to the bottom. The overdraw keeps a viewport's
                // worth of offscreen rows measured so scrolling doesn't pop.
                let state = ListState::new(0, ListAlignment::Bottom, px(512.));

                state.set_follow_mode(FollowMode::Tail);

                state
            },
            rows: Vec::new(),
            transcript_font: Default::default(),
            preview: ImagePreviewLayer::default(),
            picker: PickerReservation::default(),
            disclosures: Disclosures::new(folds_turns(collapse_mode)),
            collapse_mode,
            code_transcripts: CodeTranscriptCache::default(),
            typing: ReplyTyping::new(),
            cwd,
            kind,
            source_revision: None,
            observed_version: (0, 0),
            owner: None,
            attribution: HashMap::new(),
        }
    }

    /// Claim this view as one pane's own conversation, which is what makes its
    /// rows offer the actions that address the conversation.
    pub(crate) fn sync_content(&mut self) {
        let shared = self.conversation.clone();
        let conversation = shared.borrow();
        let version = conversation.version();

        let Some(change) = conversation.changes_since(self.observed_version) else {
            return;
        };

        if version.0 != self.observed_version.0 {
            self.reset_presentation();
        } else if let Some((index, previous_bytes)) = change.reply {
            if let SessionItem::AgentMessage {
                text: Some(text), ..
            } = &conversation.content.entries()[index].item
                && let Some(previous) =
                    self.typing
                        .begin(index, text, previous_bytes, Instant::now())
            {
                self.row_cache.invalidate(previous);
            }

            self.code_transcripts.invalidate(index);
        }

        self.code_transcripts.invalidate_from(change.first);

        self.row_cache.invalidate(change.first);

        self.observed_version = version;
    }

    pub(crate) fn set_owner(&mut self, owner: gpui::WeakEntity<AgentPane>) {
        self.owner = Some(owner);
    }

    pub(crate) fn clear_owner(&mut self) {
        self.owner = None;
    }

    pub(crate) fn owner(&self) -> Option<&gpui::WeakEntity<AgentPane>> {
        self.owner.as_ref()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.conversation.borrow().content.entries().is_empty()
    }

    pub fn attach_content(
        &mut self,
        conversation: Rc<RefCell<ConversationState>>,
        cx: &mut Context<Self>,
    ) {
        if Rc::ptr_eq(&self.conversation, &conversation) {
            self.sync_content();

            return;
        }

        self.conversation = conversation;

        self.reset_presentation();

        self.observed_version = self.conversation.borrow().version();

        self.row_cache.invalidate(0);

        cx.notify();
    }

    pub(crate) fn show_attributed_entries(
        &mut self,
        entries: Vec<Entry>,
        attribution: HashMap<String, TranscriptAttribution>,
        first_changed: usize,
        cx: &mut Context<Self>,
    ) {
        let mut conversation = self.conversation.borrow_mut();

        let moved = conversation
            .content
            .entries()
            .iter()
            .zip(&entries)
            .any(|(old, new)| old.item.id() != new.item.id());

        conversation.content.replace(entries);

        conversation.changed(first_changed, None);

        drop(conversation);

        if moved {
            self.disclosures.clear();

            self.image_previews.borrow_mut().clear();
        }

        self.attribution = attribution;

        self.sync_content();

        cx.notify();
    }

    pub(crate) fn reset_presentation(&mut self) {
        self.preview.image_preview = ImagePreview::Closed;

        self.image_previews.borrow_mut().clear();

        self.row_cache.invalidate(0);

        self.source_revision = None;
        self.picker.stashed_position = None;
        self.picker.reserve_below = false;

        self.scroll_to_bottom();

        self.disclosures.clear();

        self.code_transcripts.clear();

        self.typing.finish();
    }

    /// The part of reply `index` the reader sees this frame: the whole of it
    /// unless its edge is still crossing the text.
    pub(crate) fn shown_reply<'a>(&self, index: usize, text: &'a str) -> &'a str {
        self.typing.shown_reply(index, text)
    }

    /// How far the typed edge of a reply is from the text behind it, as the
    /// height-relevant part of that row's signature. The row lays out to what
    /// the edge lets through, so the signature has to move with the edge for
    /// the list to remeasure the row as it grows.
    #[cfg(test)]
    pub(crate) fn typed_edge(&self, index: usize) -> Option<usize> {
        self.typing.typed_edge(index)
    }

    pub(super) fn finish_typing(&mut self) {
        if let Some(index) = self.typing.finish() {
            self.row_cache.invalidate(index);
        }
    }

    pub(super) fn advance_typing(&mut self, now: Instant) -> bool {
        if !self.typing.is_typing() {
            return false;
        }

        let _profile = Probe::start(Operation::Typewriter);
        let conversation = self.conversation.borrow();

        let Some(step) = self.typing.advance(now, |index| {
            reply_chars(conversation.content.entries(), index)
        }) else {
            return false;
        };

        if step.remeasure {
            self.row_cache.invalidate(step.index);
        }

        step.moving
    }

    #[cfg(test)]
    pub(crate) fn latest_agent_message(&self, turn: u64) -> Option<String> {
        self.conversation
            .borrow()
            .content
            .latest_agent_message(turn)
            .map(str::to_owned)
    }

    /// How many actions `turn` has taken: the tool calls, file changes and
    /// thinking passes it logged. Conversation text is the turn talking rather
    /// than working, so it does not count.
    pub(crate) fn turn_steps(&self, turn: u64) -> usize {
        self.conversation.borrow().content.turn_steps(turn)
    }

    /// Completed and total entries of the task list the agent is working from.
    /// Only the newest list counts: a task list is republished in full whenever
    /// it changes, so the earlier ones describe states the agent has left.
    pub(crate) fn task_tally(&self) -> Option<(u32, u32)> {
        self.conversation.borrow().content.task_tally()
    }

    pub(crate) fn is_working(&self) -> bool {
        self.conversation.borrow().live.is_working()
    }

    pub(crate) fn start_working(&mut self, cx: &mut Context<Self>) {
        if !self.conversation.borrow().live.is_working() {
            self.conversation.borrow_mut().start();
        }

        self.row_cache
            .invalidate(self.conversation.borrow().content.entries().len());

        cx.notify();
    }

    /// Discard a turn that never produced visible output, so an immediate stop
    /// leaves no elapsed-time row behind for work that did not happen.
    pub(crate) fn discard_turn(&mut self, turn: u64, cx: &mut Context<Self>) {
        self.conversation.borrow_mut().live.discard();

        self.conversation.borrow_mut().turns.forget(turn);

        self.invalidate_turn_rows(turn);

        cx.notify();
    }

    pub(crate) fn zoom_image(
        &mut self,
        image: Arc<Image>,
        origin: Option<Bounds<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        self.preview.zoom_image(image, origin, cx);
    }

    pub(crate) fn hold_for_picker(&mut self) {
        self.picker.hold_for_picker(&self.transcript_list);
    }

    pub(crate) fn release_from_picker(&mut self, cx: &mut Context<Self>) {
        self.picker.release_from_picker(&self.transcript_list, cx);
    }

    /// Build the element for one visible row. Row indices come from the list
    /// element during layout/paint, resolved through the spec snapshot taken
    /// in the current render pass.
    pub(crate) fn render_row(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(TranscriptRow { spec, gap }) = self.rows.get(ix).cloned() else {
            return div().into_any_element();
        };

        // The list lays each row out on its own, so a run's grouping rule is
        // drawn per row rather than around the run. A segment has to carry
        // the gap below it or consecutive segments meet with a break between
        // them, and a run continues past a boundary exactly when that
        // boundary is step-ranked. The wider ranks all end the run, so their
        // space belongs below the rule rather than inside it.
        let in_run = is_run_row(&spec);
        let rule_carries_gap = in_run && gap == RowGap::Step;
        let gap = gap_px(gap);

        // The rows a run or a fold splices in open and shut as list rows of
        // their own, so each one ramps its own height and needs a height of
        // its own to ramp towards.
        let part = revealed_part(&spec);
        let now = Instant::now();

        let row = match spec {
            RowSpec::Entry { index, .. } => self.render_entry_row(index, window, cx),
            RowSpec::Work { index, .. } => self.render_work_row(index, window, cx),
            RowSpec::TurnFold {
                turn,
                row_count,
                folded,
            } => render_turn_fold(&self.disclosures, turn, row_count, folded, cx),
            RowSpec::TurnSummary {
                seconds,
                output_tokens,
            } => render_turn_summary(seconds, output_tokens, cx),
            RowSpec::Interrupted { output_tokens, .. } => render_interrupted_row(output_tokens, cx),
            RowSpec::RunToggle {
                run_start,
                tool_count,
                expanded,
            } => render_run_toggle(&self.disclosures, run_start, tool_count, expanded, cx),
            RowSpec::Working { compacting } => {
                let conversation = self.conversation.borrow();

                match conversation.live.started() {
                    Some(started) => working_row(
                        started,
                        conversation.live.output_tokens(),
                        conversation.live.detail(),
                        compacting,
                        cx,
                    ),
                    None => div().into_any_element(),
                }
            }
        };

        // Each row is laid out on its own by the virtual list, so the reading
        // column has to be re-established per row rather than once around the
        // conversation.
        let body = div()
            .w_full()
            .when(in_run, |this| {
                this.border_l(px(TRANSCRIPT_RUN_RULE))
                    .border_color(cx.theme().border)
            })
            .when(rule_carries_gap, |this| this.pb(px(gap)))
            .child(row);

        // A border is drawn at the element's own leading edge, outside any
        // padding it carries, so the inset that puts the rule on the text
        // column has to come from a level above it. Only a run needs one,
        // and only a run pays for it.
        let row = transcript_column(
            match in_run {
                true => div()
                    .w_full()
                    .pl(px(TRANSCRIPT_TEXT_INSET))
                    .child(body)
                    .into_any_element(),
                false => body.into_any_element(),
            },
            cx,
        )
        .when(!rule_carries_gap, |this| this.pb(px(gap)));

        // A row a run toggle or a turn fold spliced in grows and shrinks
        // rather than appearing and vanishing, so the conversation below it
        // travels with it the whole way instead of catching up in one jump at
        // the end. The ramp wraps the row entire — its slice of the grouping
        // rule and the space it owes the row below it — because a rule drawn
        // down to a step of no height, or a gap left where a step used to be,
        // is the part that would still jump.
        let conversation = self.conversation.borrow();

        let geometry = RowGeometry {
            rows: &self.rows,
            source: self.row_source(&conversation),
        };

        match (part, geometry.revealed_by(ix, now)) {
            (Some(part), Some(key)) => revealed_block(
                row,
                part,
                self.disclosures.progress(key, now),
                self.disclosures.height(part),
                geometry.shut_height(ix, key, now),
                cx.entity().downgrade(),
            )
            .into_any_element(),
            _ => row.into_any_element(),
        }
    }

    /// The live progress line. While the backend is compacting it names that
    /// explicitly and spins: compaction produces no streamed output, so a bare
    /// seconds counter would read as a hung turn for as long as a minute.
    pub(crate) fn render_entry_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shared = self.conversation.clone();
        let conversation = shared.borrow();
        let entry = &conversation.content.entries()[index];

        match &entry.item {
            SessionItem::UserMessage { text: Some(text) } => {
                let caps = self.kind.caps();

                // Resolved now rather than when the menu opens: a prompt's
                // place among the turns is a property of the transcript as it
                // stands, and the rows can move under a menu that is already up.
                let target = self
                    .owner()
                    .filter(|_| caps.session_fork || caps.file_rewind)
                    .zip(self.prompt_target(index))
                    .map(|(owner, target)| (owner.clone(), target));

                let menu =
                    prompt_row_menu(cx.entity().downgrade(), index, caps.session_fork, target);

                user_prompt_row(
                    index,
                    text,
                    &self.disclosures,
                    self.entry_thumbnails(index),
                    entry.metadata.at,
                    menu,
                    cx,
                )
            }
            SessionItem::AgentMessage {
                id,
                text: Some(text),
                questions: Some(questions),
            } => {
                let reply = self.render_agent_row(index, text.clone(), cx);

                question_message(
                    index,
                    id.clone(),
                    questions.clone(),
                    reply,
                    self.owner().cloned(),
                    cx,
                )
            }
            SessionItem::AgentMessage {
                text: Some(text), ..
            } => self.render_agent_row(index, self.shown_reply(index, text).to_string(), cx),
            SessionItem::Error { text } => error_row(index, text.clone(), cx),
            SessionItem::Compaction { detail, .. } => {
                let detail = detail.clone();

                compaction_row::render_compaction_row(
                    self.kind,
                    self.cwd.as_deref(),
                    &self.disclosures,
                    index,
                    detail,
                    window,
                    cx,
                )
            }
            item if is_work_row(item) => self.render_work_row(index, window, cx),
            _ => div().into_any_element(),
        }
    }

    /// An assistant reply at entry `index`, credited to its author on a
    /// conversation that mixes several agents.
    pub(crate) fn render_agent_row(
        &self,
        index: usize,
        text: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let conversation = self.conversation.borrow();
        let entry = &conversation.content.entries()[index];

        let attribution = entry.item.id().and_then(|id| self.attribution.get(id));

        let cwd = attribution.map_or_else(|| self.cwd.clone(), |author| author.cwd.clone());

        agent_reply_row(
            index,
            text,
            cwd,
            attribution.map(|author| author.name.clone()),
            entry.metadata.at,
            cx,
        )
    }

    /// Thumbnails of the images entry `index` carried, decoded once and kept
    /// for as long as the conversation is on screen.
    fn entry_thumbnails(&self, index: usize) -> Vec<Arc<Image>> {
        let conversation = self.conversation.borrow();

        let Some(entry) = conversation.content.entries().get(index) else {
            return Vec::new();
        };

        entry
            .metadata
            .images
            .iter()
            .enumerate()
            .map(|(position, image)| {
                self.image_previews
                    .borrow_mut()
                    .entry((index, position))
                    .or_insert_with(|| {
                        Arc::new(Image::from_bytes(ImageFormat::Png, image.bytes.to_vec()))
                    })
                    .clone()
            })
            .collect()
    }

    /// One step of the work log, as a card: icon block · heading · outcome
    /// mark. A collapsed card states what the step was and how it went, and
    /// nothing else — the command it ran and the output it produced are the
    /// first thing behind the disclosure. Rows with detail (command output,
    /// reasoning text) expand on click into a bounded transcript surface with
    /// its own scroll position, drawn inside the same card so the detail stays
    /// visibly attached to the step it belongs to.
    pub(crate) fn render_work_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cwd = self.cwd.clone();

        let shared = self.conversation.clone();
        let conversation = shared.borrow();
        let item = &conversation.content.entries()[index].item;

        let Some(step) = work_step(item) else {
            return div().into_any_element();
        };

        let expanded = step.detail.is_some() && self.disclosures.row_expanded(index);

        let body = step.detail.filter(|_| expanded).map(|detail| {
            if is_code_item(item) {
                let view = self.code_transcripts.ensure(index, item, cx);

                return div()
                    .w_full()
                    .modern_context_menu(copy_entry_menu(cx.entity().downgrade(), index))
                    .child(view)
                    .into_any_element();
            }

            let detail_scroll = window
                .use_keyed_state(("wl-scroll", index), cx, |_, _| ScrollHandle::default())
                .read(cx)
                .clone();

            bounded_scroll(
                &detail_scroll,
                ("wl-scrollbar", index),
                div()
                    .id(("wl-out", index))
                    .w_full()
                    .max_h(px(256.))
                    .modern_context_menu(copy_entry_menu(cx.entity().downgrade(), index))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        markdown_view(("wl-md", index), detail.to_owned(), cwd)
                            .style(transcript_text_style(cx))
                            .selectable(true),
                    ),
            )
            .into_any_element()
        });

        work_card(index, step, &self.disclosures, body, cx)
    }

    pub(super) fn append_entry(&mut self, entry: Entry) {
        let _profile = Probe::start(Operation::AppendEntry);

        // The previous last turn may gain another entry, and its final row's
        // spacing depends on the first row appended below it.
        let index = self.conversation.borrow_mut().append(entry);

        self.row_cache.invalidate(index.saturating_sub(1));
    }

    fn invalidate_turn_rows(&mut self, turn: u64) {
        if let Some(index) = self
            .conversation
            .borrow()
            .content
            .entries()
            .iter()
            .position(|entry| entry.turn == turn)
        {
            self.row_cache.invalidate(index);
        } else {
            self.row_cache
                .invalidate(self.conversation.borrow().content.entries().len());
        }
    }

    pub(super) fn refresh_rows(&mut self, collapse: CollapseRows) {
        #[cfg(test)]
        {
            self.row_cache.rebuilt_entries = 0;
        }

        if self.row_cache.mode != Some(collapse) {
            self.row_cache.mode = Some(collapse);

            self.row_cache.invalidate(0);
        }

        let Some(dirty) = self.row_cache.dirty_from.take() else {
            return;
        };

        let _profile = Probe::start(Operation::RowsRebuild);
        let shared = self.conversation.clone();
        let conversation = shared.borrow();
        let items = conversation.content.entries();

        let keep = self
            .row_cache
            .turns
            .partition_point(|(end, _)| *end <= dirty);

        self.row_cache.turns.truncate(keep);

        let (mut start, mut row_start) = self.row_cache.turns.last().copied().unwrap_or_default();

        #[cfg(test)]
        {
            self.row_cache.rebuilt_entries = items.len() - start;
        }

        let mut specs = mem::take(&mut self.row_cache.specs);

        specs.clear();

        // Reconsider the preceding row's gap along with the changed suffix.
        if row_start > 0 {
            row_start -= 1;

            specs.push(self.rows[row_start].spec.clone());
        }

        while start < items.len() {
            let turn = items[start].turn;

            let mut end = start + 1;

            while end < items.len() && items[end].turn == turn {
                end += 1;
            }

            RowSource {
                conversation: &conversation,
                disclosures: &self.disclosures,
                typing: &self.typing,
            }
            .turn_specs(turn, start, end, collapse, &mut specs);

            self.row_cache.turns.push((end, row_start + specs.len()));

            start = end;
        }

        if self.conversation.borrow().live.is_working() {
            specs.push(RowSpec::Working {
                compacting: self.conversation.borrow().live.is_compacting(),
            });
        }

        self.sync_transcript_tail(row_start, &specs);

        specs.clear();

        self.row_cache.specs = specs;
    }

    /// Flip one disclosure open or shut, holding the reader's place while its
    /// content changes and setting that content moving either way.
    ///
    /// The list stores its live end as a sentinel meaning "wherever the end
    /// now is", so rows appearing above that sentinel would carry the view
    /// down with them and take the row the reader just clicked out from under
    /// their cursor. Naming the position first pins it. A reader already
    /// sitting at the live end keeps following it, because a conversation
    /// growing under an open disclosure should still scroll itself.
    ///
    /// Shutting leaves the expanded state in place and only starts the exit;
    /// [`Self::settle_shut_disclosures`] takes the content down once there is
    /// no exit left to run.
    pub(crate) fn toggle_disclosure(&mut self, key: RevealKey, cx: &mut Context<Self>) {
        self.invalidate_disclosure_rows(key);

        if !self.transcript_list.is_following_tail() {
            self.transcript_list.freeze_scroll_position();
        }

        // Reduced motion is read here rather than where progress is reported:
        // a disclosure that never records a start has nothing in flight, so
        // the motion, the chevron's turn and the frames the transcript asks
        // for all fall away together while the pinning stays.
        let reduce_motion = cx.global::<AgentSettings>().reduce_motion;
        let now = Instant::now();

        // A disclosure part-way through its exit is still on screen but is on
        // its way out, so the click that catches it there is asking for it
        // back rather than asking again for what it is already doing.
        match self.disclosures.is_disclosing(key) {
            true if reduce_motion => self.take_down_disclosure(key),
            true => self.disclosures.begin_close(key, now),
            false => self.disclosures.open(key, now, !reduce_motion),
        }

        cx.notify();
    }

    /// Remove a shut disclosure's content and everything measured or cached
    /// for it. Splitting this from the click is what gives the exit something
    /// to move; by the time it runs there is nothing left on screen to lose.
    pub(crate) fn take_down_disclosure(&mut self, key: RevealKey) {
        self.invalidate_disclosure_rows(key);

        // The rows a run or a fold spliced in are measured a row at a time,
        // and those rows leave the list with it. Their heights are read off
        // the rows still standing, which is why they are collected before the
        // disclosure stops reporting itself as open.
        let parts = {
            let conversation = self.conversation.borrow();

            RowGeometry {
                rows: &self.rows,
                source: self.row_source(&conversation),
            }
            .revealed_parts(key)
        };

        // A closed row's segmented source would otherwise keep a second copy
        // of a large output resident behind a row showing none of it.
        if let Some(index) = self.disclosures.take_down(key, &parts) {
            self.code_transcripts.drop_row(index);
        }
    }

    fn invalidate_disclosure_rows(&mut self, key: RevealKey) {
        match key {
            RevealKey::Turn(turn) => self.invalidate_turn_rows(turn),
            RevealKey::Row(index) | RevealKey::Annotation(index) | RevealKey::Group(index) => {
                self.row_cache.invalidate(index);
            }
        }
    }

    /// Take down every disclosure whose exit has finished, re-pinning the
    /// reading position first.
    ///
    /// The pin from the click has held through the exit, but a run's rows
    /// leave the list here rather than there, and the reader may have scrolled
    /// in between. Naming the position against the layout this frame is built
    /// on is what keeps that removal from moving it.
    pub(crate) fn settle_shut_disclosures(&mut self, now: Instant) {
        let shut = self.disclosures.shut(now);

        if shut.is_empty() {
            return;
        }

        if !self.transcript_list.is_following_tail() {
            self.transcript_list.freeze_scroll_position();
        }

        for key in shut {
            self.take_down_disclosure(key);
        }
    }

    pub(crate) fn transcript_has_hidden_content_below(&self) -> bool {
        should_show_jump_to_latest(
            self.transcript_list.is_following_tail(),
            self.transcript_list.is_scrolled_to_end(),
            self.transcript_list.max_offset_for_scrollbar().y,
        )
    }

    /// Re-engaging tail mode also scrolls to the very end (past the last
    /// item), which stays correct while the last row is still growing.
    pub(crate) fn scroll_to_bottom(&self) {
        self.transcript_list.set_follow_mode(FollowMode::Tail);
    }

    /// Slide down to the live end rather than arriving there at once, so a
    /// reader who was catching up on an earlier turn sees which direction the
    /// conversation moved instead of finding a different screen of text in
    /// front of them. The list takes the tail back once the slide lands.
    pub(crate) fn glide_to_bottom(&self) {
        self.transcript_list.scroll_to_end_smooth();
    }

    /// Append an item and the images it carries, which only a user message
    /// has any of.
    pub(crate) fn push(
        &mut self,
        turn: u64,
        item: SessionItem,
        images: Vec<Arc<Image>>,
        cx: &mut Context<Self>,
    ) {
        // Submitting a user message explicitly returns to the live tail.
        // Agent output preserves a manually chosen reading position via the
        // list's own tail-follow state.
        if matches!(&item, SessionItem::UserMessage { .. }) {
            self.scroll_to_bottom();
        }

        self.append_entry(Entry {
            turn,
            item,
            metadata: EntryPresentation {
                at: Some(Utc::now().timestamp()),
                images: images
                    .into_iter()
                    .map(|image| Arc::new(ConversationImage::new(image.bytes().into())))
                    .collect(),
            },
        });

        cx.notify();
    }

    /// Data-only description of every transcript row, in render order. This
    /// is the single source of truth for the transcript's structure; the
    /// virtualized list builds elements only for the visible slice of it.
    #[cfg(test)]
    pub(crate) fn build_row_specs(&self, collapse: CollapseRows) -> Vec<RowSpec> {
        let conversation = self.conversation.borrow();

        self.row_source(&conversation).all_specs(collapse)
    }

    /// The row structure's source as this view holds it.
    fn row_source<'a>(&'a self, conversation: &'a ConversationState) -> RowSource<'a> {
        RowSource {
            conversation,
            disclosures: &self.disclosures,
            typing: &self.typing,
        }
    }

    /// The disclosure whose ramp list row `ix` travels on this frame.
    #[cfg(test)]
    pub(crate) fn revealed_by(&self, ix: usize, now: Instant) -> Option<RevealKey> {
        let conversation = self.conversation.borrow();

        RowGeometry {
            rows: &self.rows,
            source: self.row_source(&conversation),
        }
        .revealed_by(ix, now)
    }

    /// Diff freshly built specs against the list's current contents and
    /// notify it as narrowly as possible: an equal-count middle means rows
    /// changed in place (streaming growth, expansion) and only needs
    /// remeasuring, which preserves the scroll position exactly; a count
    /// change is a real splice.
    #[cfg(test)]
    pub(crate) fn sync_transcript_list(&mut self, new: Vec<RowSpec>) {
        self.sync_transcript_tail(0, &new);
    }

    fn sync_transcript_tail(&mut self, start: usize, specs: &[RowSpec]) {
        let mut new = mem::take(&mut self.row_cache.scratch_rows);

        spaced_rows(
            self.conversation.borrow().content.entries(),
            specs,
            &mut new,
        );

        if self.rows[start..] == new {
            new.clear();

            self.row_cache.scratch_rows = new;

            return;
        }

        let prefix = self.rows[start..]
            .iter()
            .zip(&new)
            .take_while(|(a, b)| a == b)
            .count();

        let suffix = self.rows[start + prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();

        let old_mid = start + prefix..self.rows.len() - suffix;
        let new_mid = &new[prefix..new.len() - suffix];

        if old_mid.len() == new_mid.len() {
            self.transcript_list.remeasure_items(old_mid);
        } else {
            // A row next to one arriving or leaving changes with it, because
            // its gap is ranked against its neighbour, yet it is still the
            // row the list holds. Splicing it would zero its height in the
            // scrollbar's total until it is next laid out and reset a reading
            // position inside it to its top, so only the rows that really
            // came or went are spliced.
            let same_row =
                |(old, new): &(&TranscriptRow, &TranscriptRow)| old.spec.is_same_row(&new.spec);

            let kept_above = self.rows[old_mid.clone()]
                .iter()
                .zip(new_mid)
                .take_while(same_row)
                .count();

            let kept_below = self.rows[old_mid.start + kept_above..old_mid.end]
                .iter()
                .rev()
                .zip(new_mid[kept_above..].iter().rev())
                .take_while(same_row)
                .count();

            let replaced = old_mid.start + kept_above..old_mid.end - kept_below;

            self.transcript_list
                .remeasure_items(old_mid.start..replaced.start);

            self.transcript_list
                .remeasure_items(replaced.end..old_mid.end);

            self.transcript_list
                .splice(replaced, new_mid.len() - kept_above - kept_below);
        }

        self.rows.truncate(start);

        self.rows.append(&mut new);

        self.row_cache.scratch_rows = new;
    }

    /// Name the branch point one transcript row points at, for the row's own
    /// menu. `None` where the row is not a prompt that opened a turn, which is
    /// a row no cut can be anchored on.
    pub(crate) fn prompt_target(&self, index: usize) -> Option<PromptTarget> {
        let conversation = self.conversation.borrow();

        let SessionItem::UserMessage { text: Some(prompt) } =
            &conversation.content.entries().get(index)?.item
        else {
            return None;
        };

        let openings = turn_opening_prompts(self.conversation.borrow().content.entries());
        let position = openings.iter().position(|opening| *opening == index)?;

        Some(PromptTarget {
            prompt: prompt.clone(),
            depth: openings.len() - 1 - position,
        })
    }

    /// The transcript row a branch point names, found the way `prompt_target`
    /// names one: counted back from the newest turn-opening prompt, with the
    /// text confirming the count landed on the same message. `None` where the
    /// two disagree, which is a row the transcript should not be moved to.
    pub(crate) fn prompt_row(&self, target: &PromptTarget) -> Option<usize> {
        let openings = turn_opening_prompts(self.conversation.borrow().content.entries());
        let index = *openings.get(openings.len().checked_sub(target.depth + 1)?)?;

        let conversation = self.conversation.borrow();

        let SessionItem::UserMessage { text: Some(prompt) } =
            &conversation.content.entries()[index].item
        else {
            return None;
        };

        if *prompt != target.prompt {
            return None;
        }

        self.rows
            .iter()
            .position(|row| matches!(row.spec, RowSpec::Entry { index: at, .. } if at == index))
    }

    /// Put the prompt a branch point names at the top of the transcript, so
    /// the conversation follows the row a picker is highlighting.
    ///
    /// Top rather than merely visible: a picker floats over the bottom of the
    /// transcript, so a prompt revealed at the lower edge would be hidden
    /// behind the list naming it — and the turns the cut would discard are
    /// what the user is deciding about, which is what sits below it.
    pub(crate) fn scroll_to_prompt(
        &self,
        target: &PromptTarget,
        smooth: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.prompt_row(target) else {
            return;
        };

        let offset = ListOffset {
            item_ix: row,
            offset_in_item: px(0.),
        };

        if smooth {
            self.transcript_list.scroll_to_smooth(offset);
        } else {
            self.transcript_list.scroll_to(offset);
        }

        cx.notify();
    }
}

/// Length in characters of the reply at `index`, which is what its typed
/// edge closes on.
fn reply_chars(items: &[Entry], index: usize) -> usize {
    match items.get(index).map(|entry| &entry.item) {
        Some(SessionItem::AgentMessage {
            text: Some(text), ..
        }) => text.chars().count(),
        _ => 0,
    }
}

/// Empty space left below the conversation while a picker follows it.
///
/// What has to be cleared is the picker itself, which floats over the bottom
/// of the pane, so the room it can cover is the room to leave. A viewport too
/// short for that keeps a screenful of conversation instead: a list padded to
/// its own height has no space left to paint rows in, and would go blank.
fn picker_reserve(viewport: Pixels) -> Pixels {
    const KEEP_VISIBLE: Pixels = px(120.);

    PALETTE_MAX_HEIGHT.min((viewport - KEEP_VISIBLE).max(px(0.)))
}

impl Render for TranscriptView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_content();

        // Content whose exit has finished leaves the transcript before the
        // rows are built, so the removal and the specs it changes land in one
        // pass rather than a frame apart.
        let now = Instant::now();

        self.settle_shut_disclosures(now);

        // A disclosure moves from a click, which wakes the frame pump once.
        // Keeping it awake for the rest of the motion is this view's own job:
        // without the request the remaining frames arrive only when something
        // unrelated repaints, so a conversation nobody is typing into would
        // show the first frame of the motion and stop there.
        if !self.disclosures.settled(now) {
            window.request_animation_frame();
        }

        // The typed edge moves once per frame, before the rows are built, so
        // the row signatures and the prefix they are measured against describe
        // the same frame. The stream wakes the pump as each chunk lands; the
        // frames between chunks are this view's to ask for. Reduced motion
        // shows what has arrived as it arrives.
        let reduce_motion = cx.global::<AgentSettings>().reduce_motion;

        if reduce_motion {
            self.finish_typing();
        } else if self.advance_typing(now) {
            window.request_animation_frame();
        }

        let settings = cx.global::<AgentSettings>();
        let collapse_tool_calls = settings.collapse_tool_calls;
        let smooth_wheel = settings.smooth_wheel;

        let font = (
            settings.font_family.clone(),
            settings.font_size,
            settings.transcript_font_family.clone(),
            settings.transcript_font_size,
        );

        // Switching the setting is a directive about the whole transcript, so
        // it drops the folds and expansions the previous mode collected. They
        // record a departure from a default that just moved: kept, they would
        // open exactly the turns the user had folded and fold the ones they
        // had opened, which reads as the setting doing the opposite of what it
        // says.
        if self.collapse_mode != collapse_tool_calls {
            self.collapse_mode = collapse_tool_calls;

            // Anything mid-exit goes with its own state: dropping the reveal
            // alone would strand the disclosure open with nothing left to
            // finish shutting it.
            for key in self.disclosures.closing() {
                self.take_down_disclosure(key);
            }

            self.disclosures
                .forget_departures(folds_turns(collapse_tool_calls));
        }

        self.transcript_list.set_smooth_wheel_enabled(smooth_wheel);

        // Transcript rows, one folded/expanded section per turn (entries are
        // tagged with a monotonic turn id, so turns are contiguous slices).
        // Only the visible slice becomes elements; the spec diff tells the
        // list which rows changed shape.
        self.refresh_rows(collapse_tool_calls);

        if self.transcript_font != font {
            self.transcript_font = font;

            self.transcript_list.remeasure();
        }

        // The reserve is measured from the previous layout, which is the
        // viewport the next one will use unless the window is being resized.
        let reserve_below = self
            .picker
            .reserve_below
            .then_some(self.preview.transcript_height)
            .flatten()
            .map(picker_reserve);

        let has_hidden_content_below = self.transcript_has_hidden_content_below();

        // The scrollbar must sit OUTSIDE the scrolling element (a child would
        // scroll away with the content), so a relative wrapper hosts the
        // scroll area and the overlay bar.
        div()
            .relative()
            .size_full()
            .min_h_0()
            // Set here rather than on each row: every row of a conversation
            // shares one leading, including the ones built from Markdown that
            // never see the row builder's own styles.
            .line_height(relative(TRANSCRIPT_LINE_HEIGHT))
            .child(
                div()
                    .id("agent-transcript")
                    .size_full()
                    .on_prepaint({
                        let view = cx.entity().downgrade();

                        move |bounds, _, cx| {
                            view.update(cx, |this, cx| {
                                this.preview.transcript_height = Some(bounds.size.height);
                                this.preview.transcript_origin = Some(bounds.origin);

                                let width = bounds.size.width;

                                if this.preview.transcript_width != Some(width) {
                                    this.preview.transcript_width = Some(width);

                                    this.transcript_list.remeasure();

                                    cx.notify();
                                }
                            })
                            .ok();
                        }
                    })
                    .child(
                        // Element callbacks run after this render's entity
                        // lease is released, so the row builder re-enters
                        // through a weak handle.
                        list(self.transcript_list.clone(), {
                            let this = cx.entity().downgrade();

                            move |ix, window, cx| {
                                this.update(cx, |this, cx| this.render_row(ix, window, cx))
                                    .unwrap_or_else(|_| div().into_any_element())
                            }
                        })
                        .size_full()
                        .pt(px(16.))
                        .when_some(reserve_below, |this, reserve| this.pb(reserve)),
                    ),
            )
            // The bare Scrollbar element carries no inset of its own, so it
            // lands at its static flow position (below the full-height
            // sibling); the pinned strip gives it a deterministic containing
            // block at the right edge.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(16.))
                    .child(Scrollbar::vertical(&self.transcript_list)),
            )
            .when(has_hidden_content_below, |this| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(px(12.))
                        .flex()
                        .justify_center()
                        .child(
                            // The button floats over the conversation, and its
                            // own fill carries the theme's alpha, so the text
                            // underneath reads through it. Button paints that
                            // fill during its own render, after any background
                            // set from here, so the opaque surface has to be a
                            // layer behind it rather than a style on it.
                            div()
                                .rounded(UI_RADIUS)
                                .overflow_hidden()
                                .bg(cx.theme().background.alpha(1.0))
                                .shadow_md()
                                .child(
                                    // Button::small() hard-codes h_6 during
                                    // render, which would overwrite any height
                                    // set here; min_h clamps the final layout
                                    // instead.
                                    Button::new("agent-jump-to-bottom")
                                        .small()
                                        .min_h(px(36.))
                                        .rounded(UI_RADIUS)
                                        .icon(IconName::ArrowDown)
                                        .label(t!("agent-transcript-scroll-bottom"))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            // A click arrives outside a frame,
                                            // and the slide only starts on the
                                            // next layout, so the notify is
                                            // what wakes the pump for it.
                                            if cx.global::<AgentSettings>().reduce_motion {
                                                this.scroll_to_bottom();
                                            } else {
                                                this.glide_to_bottom();
                                            }

                                            cx.notify();
                                        })),
                                ),
                        ),
                )
            })
            .children(self.preview.render_zoomed_image(now, window, cx))
    }
}
