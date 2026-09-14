#[cfg(test)]
mod tests;
#[cfg(test)]
mod typewriter_tests;

use crate::agent_tab::AgentPane;
use crate::agent_tab::capabilities::AgentCapabilities as _;
use crate::agent_tab::composer::attachments::MAX_ATTACHMENTS;
use crate::agent_tab::composer::{
    PALETTE_MAX_HEIGHT, PromptTarget, annotation_count_label, parse_annotated_prompt,
};
use crate::agent_tab::profile::AgentKind;
use crate::agent_tab::settings::{AgentSettings, UI_RADIUS};
use crate::agent_tab::transcript::code::is_code_item;
use crate::agent_tab::transcript::disclosure_row::{
    AGENT_CARD_BODY_PADDING_Y, AGENT_CARD_DETAIL_SIZE, AGENT_CARD_GAP, AGENT_CARD_ICON_BLOCK,
    AGENT_CARD_PADDING_X, AGENT_CARD_RADIUS, AGENT_DISCLOSURE_DETAIL_INSET, AgentCardTone,
    AgentDisclosureRow, USER_ANNOTATION_PADDING_Y, USER_BUBBLE_PADDING_X, USER_BUBBLE_PADDING_Y,
    USER_BUBBLE_RADIUS, USER_BUBBLE_TAIL_RADIUS, USER_BUBBLE_WIDTH_FRACTION, agent_card,
};
use crate::agent_tab::transcript::incremental::RowCache;
use crate::agent_tab::transcript::render::image_preview::{ImagePreview, ImagePreviewLayer};
use crate::agent_tab::transcript::render::text_style::{markdown_view, transcript_text_style};
use crate::agent_tab::transcript::render::{
    TRANSCRIPT_LINE_HEIGHT, TRANSCRIPT_RUN_RULE, TRANSCRIPT_TEXT_INSET, TRANSCRIPT_THUMBNAIL,
    WorkingIndicator, compaction_row, gap_px, render_interrupted_row, render_run_toggle,
    render_turn_fold, render_turn_summary, transcript_column,
};
use crate::agent_tab::transcript::reveal::{
    Disclosures, RevealKey, RevealedPart, revealed, revealed_block, revealed_part,
};
use crate::agent_tab::transcript::rows::{
    EntryPresentation, PickerReservation, RowGap, TranscriptRow, TurnSummary, entry_fingerprint,
    folds_turns, is_run_row, row_gap, spaced_rows, turn_opening_prompts, turn_summary,
};
use crate::agent_tab::transcript::{
    CodeTranscriptCache, Entry, RowSpec, command_execution_heading, command_failure_reason,
    entry_copy_text, hidden, is_work_row, should_show_jump_to_latest, truncated_user_prompt,
    working_label,
};
use chrono::{DateTime, Local, Utc};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, Div, FollowMode, FontWeight, Image,
    ImageFormat, IntoElement, ListAlignment, ListOffset, ListState, ObjectFit, Pixels, Render,
    ScrollHandle, SharedString, Window, div, img, list, px, relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::modern_menu::{ModernMenu, ModernMenuExt as _};
use gpui_component::scroll::Scrollbar;
use gpui_component::shimmer::ShimmerText;
use gpui_component::spinner::Spinner;
use gpui_component::{
    ActiveTheme as _, ElementExt as _, Icon, IconName, Sizable as _, h_flex, text, v_flex,
};
use nmt_agent::chat::{Item as SessionItem, Question};
use nmt_agent::transcript::conversation::{ConversationImage, ConversationState};
use nmt_config::agent::CollapseRows;
use nmt_profiling::transcript::{Operation, Probe};
use rust_i18n::t;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::mem;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

    /// The reply being let onto the screen a character at a time, while one
    /// is. Only text that streams in through this view is typed: a restored
    /// or mirrored conversation arrives whole and is shown whole.
    typewriter: Option<Typewriter>,

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
            typewriter: None,
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
            if !self
                .typewriter
                .as_ref()
                .is_some_and(|typing| typing.index() == index)
            {
                if let Some(previous) = &self.typewriter {
                    self.row_cache.invalidate(previous.index());
                }

                if let SessionItem::AgentMessage {
                    text: Some(text), ..
                } = &conversation.content.entries()[index].item
                {
                    self.typewriter = Some(Typewriter::start(
                        index,
                        text[..previous_bytes].chars().count(),
                        Instant::now(),
                    ));
                }
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
        self.typewriter = None;
    }

    /// The part of reply `index` the reader sees this frame: the whole of it
    /// unless its edge is still crossing the text.
    pub(crate) fn shown_reply<'a>(&self, index: usize, text: &'a str) -> &'a str {
        match &self.typewriter {
            Some(typewriter) if typewriter.index() == index => {
                shown_prefix(text, typewriter.shown())
            }

            _ => text,
        }
    }

    /// How far the typed edge of a reply is from the text behind it, as the
    /// height-relevant part of that row's signature. The row lays out to what
    /// the edge lets through, so the signature has to move with the edge for
    /// the list to remeasure the row as it grows.
    pub(crate) fn typed_edge(&self, index: usize) -> Option<usize> {
        self.typewriter
            .as_ref()
            .filter(|typewriter| typewriter.index() == index)
            .map(Typewriter::shown)
    }

    pub(super) fn finish_typing(&mut self) {
        if let Some(typewriter) = self.typewriter.take() {
            self.row_cache.invalidate(typewriter.index());
        }
    }

    pub(super) fn advance_typing(&mut self, now: Instant) -> bool {
        let Some(typewriter) = &mut self.typewriter else {
            return false;
        };

        let _profile = Probe::start(Operation::Typewriter);
        let index = typewriter.index();
        let previous = typewriter.shown();

        let moving = typewriter.advance(
            reply_chars(self.conversation.borrow().content.entries(), index),
            now,
        );

        if typewriter.shown() != previous {
            self.row_cache.invalidate(index);
        }

        if !moving {
            self.finish_typing();
        }

        moving
    }

    /// Latest non-empty assistant reply of `turn`, for notification bodies.
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

            RowSpec::Working { compacting } => self.render_working_row(compacting, cx),
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
        match (part, self.revealed_by(ix, now)) {
            (Some(part), Some(key)) => revealed_block(
                row,
                part,
                self.disclosures.progress(key, now),
                self.disclosures.height(part),
                self.shut_height(ix, key, now),
                cx.entity().downgrade(),
            )
            .into_any_element(),

            _ => row.into_any_element(),
        }
    }

    /// What a shutting row still occupies once it has finished shutting.
    ///
    /// When a block of rows leaves the list, the row above the block stops
    /// holding its space off the block's first row and starts holding it off
    /// whatever followed the block, and those two boundaries can rank apart:
    /// a toggle sits a step off its first step and a work rank off the reply
    /// after the run. The block's last row holds back exactly that
    /// difference, so the space the row above gains at the removal is the
    /// space the block gives up, and the removal itself moves nothing. Every
    /// row above the last owes nothing, because the boundary it leaves
    /// behind is inside the block.
    fn shut_height(&self, ix: usize, key: RevealKey, now: Instant) -> Pixels {
        if self.revealed_by(ix + 1, now) == Some(key) {
            return px(0.);
        }

        let first = (0..ix)
            .rev()
            .take_while(|cursor| self.revealed_by(*cursor, now) == Some(key))
            .last()
            .unwrap_or(ix);

        let Some(above) = first.checked_sub(1).map(|above| &self.rows[above]) else {
            return px(0.);
        };

        let below = self.rows.get(ix + 1).map(|row| &row.spec);

        let merged = row_gap(
            self.conversation.borrow().content.entries(),
            &above.spec,
            below,
        );

        px(gap_px(merged) - gap_px(above.gap))
    }

    /// The live progress line. While the backend is compacting it names that
    /// explicitly and spins: compaction produces no streamed output, so a bare
    /// seconds counter would read as a hung turn for as long as a minute.
    pub(crate) fn render_working_row(
        &self,
        compacting: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(started) = self.conversation.borrow().live.started() else {
            return div().into_any_element();
        };

        if compacting {
            let accent = cx.theme().info;

            return h_flex()
                .w_full()
                .gap(px(AGENT_CARD_GAP))
                .items_center()
                .px(px(AGENT_CARD_PADDING_X))
                .child(
                    div()
                        .size(px(AGENT_CARD_ICON_BLOCK))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            Spinner::new()
                                .icon(IconName::LoaderCircle)
                                .with_size(px(12.))
                                .color(accent),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(accent)
                                .child(t!("agent-transcript-compacting")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(working_label(
                                    started,
                                    self.conversation.borrow().live.output_tokens(),
                                    self.conversation.borrow().live.detail(),
                                )),
                        ),
                )
                .into_any_element();
        }

        // The dots stand in the slot a card gives its type icon, so the label
        // starts on the column a tool call's title starts on and the live line
        // reads as the next step of the work above it rather than as a stray
        // line under it. A ring turning in that slot reads as one more step
        // with an icon; a travelling swell reads as the pane waiting.
        h_flex()
            .w_full()
            .gap(px(AGENT_CARD_GAP))
            .items_center()
            .px(px(AGENT_CARD_PADDING_X))
            .child(
                div()
                    .size(px(AGENT_CARD_ICON_BLOCK))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(WorkingIndicator::new(cx.theme().warning)),
            )
            .child(
                div()
                    .text_size(px(AGENT_CARD_DETAIL_SIZE))
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        // The label text changes every second, so it cannot
                        // serve as the animation identity; a fixed id keeps
                        // one animation state alive across those rewrites.
                        //
                        // The band lifts the muted label to full foreground
                        // contrast. The component's theme-derived default
                        // mixes the text toward the background on light
                        // themes, which fades the band into the page instead,
                        // and its default peak leaves the muted label only
                        // slightly lifted at the twelve-pixel detail size.
                        ShimmerText::new(working_label(
                            started,
                            self.conversation.borrow().live.output_tokens(),
                            self.conversation.borrow().live.detail(),
                        ))
                        .id("agent-working-label")
                        .highlight_color(cx.theme().foreground)
                        .peak_opacity(0.9),
                    ),
            )
            .into_any_element()
    }

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
            SessionItem::UserMessage { text: Some(text) } => self.render_user_row(index, text, cx),

            SessionItem::AgentMessage {
                id,
                text: Some(text),
                questions: Some(questions),
            } => {
                self.render_question_message(index, id.clone(), text.clone(), questions.clone(), cx)
            }

            SessionItem::AgentMessage {
                text: Some(text), ..
            } => self.render_agent_row(index, self.shown_reply(index, text).to_string(), cx),

            SessionItem::Error { text } => self.render_error_row(index, text.clone(), cx),

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

    pub(crate) fn render_agent_row(
        &self,
        index: usize,
        text: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shared = self.conversation.clone();
        let conversation = shared.borrow();

        let attribution = conversation.content.entries()[index]
            .item
            .id()
            .and_then(|id| self.attribution.get(id));

        let cwd = attribution.map_or_else(|| self.cwd.clone(), |author| author.cwd.clone());

        h_flex()
            .id(("entry", index))
            .group("entry")
            .relative()
            .w_full()
            .items_end()
            .modern_context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            .child(
                v_flex()
                    .debug_selector(move || format!("transcript-agent-{index}"))
                    .flex_1()
                    .min_w_0()
                    .px_1()
                    .when_some(attribution, |view, author| {
                        view.child(
                            div()
                                .debug_selector(move || format!("transcript-author-{index}"))
                                .mb_2()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().muted_foreground)
                                .child(author.name.clone()),
                        )
                    })
                    .child(
                        markdown_view(("agent-md", index), text, cwd)
                            .style(transcript_text_style(cx))
                            .selectable(true),
                    ),
            )
            .child(
                // A stamp in the flow would reserve its width on every row,
                // ending assistant output short of the pane by a strip that is
                // blank whenever the pointer is elsewhere. Out of the flow it
                // costs nothing until it appears, and the tinted chip keeps it
                // legible where it lands over the last line.
                self.hover_stamp(index, cx)
                    .absolute()
                    .right_1()
                    .bottom_0()
                    .px_1()
                    .rounded(UI_RADIUS)
                    .bg(cx.theme().muted),
            )
            .into_any_element()
    }

    pub(crate) fn render_error_row(
        &self,
        index: usize,
        text: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .id(("entry", index))
            .w_full()
            .modern_context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            .child(
                div()
                    .max_w(relative(0.9))
                    .px_3()
                    .py_2()
                    .rounded(UI_RADIUS)
                    .bg(cx.theme().danger.opacity(0.15))
                    .text_color(cx.theme().danger)
                    .text_sm()
                    .child(text),
            )
            .into_any_element()
    }

    /// Hover-revealed timestamp; the row declares `.group("entry")`.
    pub(crate) fn hover_stamp(&self, index: usize, cx: &mut Context<Self>) -> Div {
        div()
            .flex_none()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .invisible()
            .group_hover("entry", |this| this.visible())
            .child(
                self.conversation.borrow().content.entries()[index]
                    .metadata
                    .at
                    .and_then(|at| DateTime::from_timestamp(at, 0))
                    .map(|at| at.with_timezone(&Local).format("%H:%M").to_string())
                    .unwrap_or_default(),
            )
    }

    pub(crate) fn copy_menu(
        pane: gpui::WeakEntity<Self>,
        index: usize,
    ) -> impl Fn(ModernMenu, &mut Window, &mut App) -> ModernMenu + 'static {
        move |menu, _, cx| {
            // Full transcript payloads can be very large. Resolve and clone the
            // text only after a right click opens the menu, keeping ordinary
            // list layout independent of the hidden message size.
            let copy_text = pane
                .read_with(cx, |pane, _| {
                    pane.conversation
                        .borrow()
                        .content
                        .entries()
                        .get(index)
                        .map(|entry| entry_copy_text(&entry.item))
                })
                .ok()
                .flatten();

            match copy_text {
                Some(copy_text) => menu
                    .item(t!("agent-transcript-copy"), move |_, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                    })
                    .icon(IconName::Copy),

                None => menu,
            }
        }
    }

    /// The copy item plus the actions a prompt offers over the conversation
    /// it opened: branching in front of it, or returning to it.
    ///
    /// Which of the two appears follows the backend. Where a branch is a
    /// request the harness answers, the prompt names a cut and nothing else;
    /// where the conversation is a transcript file this side rewrites, the
    /// same cut also decides what happens to the files that turn touched, so
    /// the rewind actions are what the prompt leads to.
    fn user_row_menu(
        &self,
        index: usize,
        cx: &Context<Self>,
    ) -> impl Fn(ModernMenu, &mut Window, &mut App) -> ModernMenu + 'static {
        let copy = Self::copy_menu(cx.entity().downgrade(), index);
        let caps = self.kind.caps();

        // Resolved now rather than when the menu opens: a prompt's place among
        // the turns is a property of the transcript as it stands, and the rows
        // can move under a menu that is already up.
        let target = self
            .owner()
            .filter(|_| caps.session_fork || caps.file_rewind)
            .zip(self.prompt_target(index))
            .map(|(owner, target)| (owner.clone(), target));

        move |menu, window, cx| {
            let menu = copy(menu, window, cx);

            let Some((pane, target)) = target.clone() else {
                return menu;
            };

            if caps.session_fork {
                menu.separator()
                    .item(t!("agent-transcript-fork-from-here"), move |_, cx| {
                        let target = target.clone();

                        pane.update(cx, |pane, cx| pane.fork_from_prompt(target, cx))
                            .ok();
                    })
                    .icon(IconName::GitBranch)
            } else {
                menu.separator()
                    .item(t!("agent-transcript-rewind-to-here"), move |_, cx| {
                        let target = target.clone();

                        pane.update(cx, |pane, cx| pane.rewind_to_prompt(target, cx))
                            .ok();
                    })
                    .icon(IconName::Undo)
            }
        }
    }

    /// User prompt: right-aligned quiet bubble (muted surface, no border).
    ///
    /// Oversized prompts (huge pastes) collapse to their head by default:
    /// a visible row re-lays-out its full text every frame, so an unbounded
    /// prompt would make every frame O(paste size). Expansion is an explicit
    /// per-row choice, and the right-click Copy always carries the full text.
    pub(crate) fn render_user_row(
        &self,
        index: usize,
        text: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let parsed = parse_annotated_prompt(text);
        let text = parsed.as_ref().map_or(text, |parsed| parsed.prompt);
        let head_len = truncated_user_prompt(text).map(str::len);
        let expanded = head_len.is_some() && self.disclosures.row_expanded(index);

        let shown = match (head_len, expanded) {
            (Some(len), false) => text[..len].to_string(),
            _ => text.to_string(),
        };

        // A prompt long enough to fold is a pasted block rather than a
        // sentence, and it takes the column's whole measure. Sized to its
        // content it would instead be as wide as the longest line of whichever
        // half is on screen, so opening it would move its edges as well as its
        // height; measuring the hidden half to avoid that is the layout pass
        // the fold exists to skip.
        let fills_column = head_len.is_some();

        let toggle = head_len.is_some().then(|| {
            div()
                .mt_1()
                .text_xs()
                .text_color(cx.theme().primary)
                .cursor_pointer()
                .child(if expanded {
                    t!("agent-transcript-show-less").to_string()
                } else {
                    t!("agent-transcript-show-full-message").to_string()
                })
                .id(("user-expand", index))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_disclosure(RevealKey::Row(index), cx)
                }))
        });

        // The prompt fold above swaps the text inside one bubble rather than
        // opening a block below it, so it takes no entrance of its own: fading
        // it in would fade the half of the prompt that was already on screen.
        // Its toggle still pins the reading position, which is what a paste
        // long enough to fold actually needs.
        let annotations_reveal = self
            .disclosures
            .progress(RevealKey::Annotation(index), Instant::now());

        // The quotations open a rounded bubble, and a clip box is a rectangle,
        // so they fade in place rather than growing by height: squaring off
        // the corner the bubble is known by would cost more than the height
        // ramp buys on a block this size. The card is shaped for as long as
        // they are on screen and the wording answers the click at once.
        let annotations_shown =
            self.disclosures.annotation_expanded(index) && annotations_reveal > 0.0;

        let annotations_disclosing = self.disclosures.is_disclosing(RevealKey::Annotation(index));

        let annotations = parsed.as_ref().and_then(|parsed| {
            (!parsed.annotations.is_empty()).then(|| {
                let action_label = if annotations_disclosing {
                    t!("agent-transcript-annotations-collapse")
                } else {
                    t!("agent-transcript-annotations-expand")
                };

                let content = annotations_shown.then(|| {
                    v_flex()
                        .w_full()
                        .gap_2()
                        .map(|this| revealed(this, annotations_reveal))
                        // Closes the bubble the header opens, and takes the
                        // header's own edge inset so a quotation starts on the
                        // same column the header's label does.
                        .rounded_b(px(USER_BUBBLE_RADIUS))
                        .bg(cx.theme().muted)
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .px(px(USER_BUBBLE_PADDING_X))
                        .py(px(USER_BUBBLE_PADDING_Y))
                        .children(parsed.annotations.iter().enumerate().map(
                            |(position, annotation)| {
                                h_flex()
                                    .w_full()
                                    .items_start()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("{}.", position + 1)),
                                    )
                                    .child(
                                        div().flex_1().min_w_0().child(
                                            text::TextView::plain(
                                                format!(
                                                    "entry-response-annotation-{index}-{position}"
                                                ),
                                                annotation.text.clone(),
                                            )
                                            .selectable(true),
                                        ),
                                    )
                            },
                        ))
                });

                v_flex()
                    // Sized to what it says, like the prompt below it, and
                    // right-aligned with it by the column both sit in. The
                    // quotations are the wider of the two, so opening them is
                    // what grows the bubble.
                    .min_w_0()
                    .child(
                        // A second bubble in the prompt's own language: same
                        // fill, same corner, same edge inset, quieter text.
                        // Its padding and inherited text size come from the
                        // bubble rather than from a button size, because the
                        // transcript's text size is a setting and a control
                        // with a fixed height would stop matching the bubble
                        // below it as soon as that setting moves.
                        h_flex()
                            .id(("entry-response-annotations", index))
                            .role(gpui::Role::Button)
                            .aria_label(action_label)
                            .w_full()
                            .px(px(USER_BUBBLE_PADDING_X))
                            .py(px(USER_ANNOTATION_PADDING_Y))
                            .bg(cx.theme().muted)
                            .text_color(cx.theme().muted_foreground)
                            // Squares off where the quotations meet it, and is
                            // a closed capsule while they are hidden.
                            .map(|this| match annotations_shown {
                                true => this.rounded_t(px(USER_BUBBLE_RADIUS)),
                                false => this.rounded(px(USER_BUBBLE_RADIUS)),
                            })
                            .gap_2()
                            .items_center()
                            .cursor_pointer()
                            .hover(|style| style.bg(cx.theme().accent))
                            .child(Icon::new(IconName::TextSelect).xsmall())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(annotation_count_label(parsed.annotations.len())),
                            )
                            .child(
                                Icon::new(if annotations_disclosing {
                                    IconName::ChevronUp
                                } else {
                                    IconName::ChevronDown
                                })
                                .xsmall(),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_disclosure(RevealKey::Annotation(index), cx)
                            })),
                    )
                    .children(content)
            })
        });

        let message = div()
            // The width cap lives on the column below, which has a definite
            // width to take a fraction of. A fraction here would resolve
            // against this bubble's own shrink-to-fit parent instead, wrapping
            // every prompt at a fraction of its natural single-line width.
            .min_w_0()
            .when(fills_column, |this| this.w_full())
            .px(px(USER_BUBBLE_PADDING_X))
            .py(px(USER_BUBBLE_PADDING_Y))
            .rounded_tl(px(USER_BUBBLE_RADIUS))
            .rounded_tr(px(USER_BUBBLE_RADIUS))
            .rounded_bl(px(USER_BUBBLE_RADIUS))
            // The one square-ish corner faces the conversation the prompt was
            // sent into, which is what marks the bubble as this side of it.
            .rounded_br(px(USER_BUBBLE_TAIL_RADIUS))
            .bg(cx.theme().muted)
            // Plain, not markdown: the prompt is user-authored text and
            // must render verbatim, but stays drag-selectable.
            .child(text::TextView::plain(("user-text", index), shown).selectable(true))
            .children(toggle)
            .children(self.render_entry_images(index, cx));

        h_flex()
            .id(("entry", index))
            .group("entry")
            .w_full()
            .justify_end()
            .items_end()
            .gap_2()
            .modern_context_menu(self.user_row_menu(index, cx))
            .child(self.hover_stamp(index, cx))
            .child(
                v_flex()
                    // Both bubbles size to their own content and end on this
                    // column's trailing edge, so the cap that keeps a prompt
                    // off the full width lives here rather than on either. The
                    // row above is `w_full`, so the fraction has a definite
                    // width to resolve against and tracks the pane.
                    //
                    // A foldable prompt takes that measure as its width rather
                    // than as a ceiling: a bubble asking for the full width of
                    // a shrink-to-fit column would still be sized by its own
                    // longest line, since a percentage contributes nothing to
                    // what a column asks for.
                    .map(|this| match fills_column {
                        true => this.w(relative(USER_BUBBLE_WIDTH_FRACTION)),
                        false => this.max_w(relative(USER_BUBBLE_WIDTH_FRACTION)),
                    })
                    .min_w_0()
                    .items_end()
                    .gap_1()
                    .children(annotations)
                    .child(message),
            )
            .into_any_element()
    }

    /// The images a message carried, under its text. A reader who scrolls back
    /// should see what was sent, not the placeholder that stood in for it while
    /// the message was being written.
    fn render_entry_images(&self, index: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let conversation = self.conversation.borrow();
        let images = &conversation.content.entries().get(index)?.metadata.images;

        if images.is_empty() {
            return None;
        }

        Some(
            h_flex()
                .mt_2()
                .gap_2()
                .flex_wrap()
                .justify_end()
                .children(images.iter().enumerate().map(|(position, image)| {
                    let image = self
                        .image_previews
                        .borrow_mut()
                        .entry((index, position))
                        .or_insert_with(|| {
                            Arc::new(Image::from_bytes(ImageFormat::Png, image.bytes.to_vec()))
                        })
                        .clone();

                    // A click carries the pointer's position, not the
                    // thumbnail's; the bounds the layout gave it are kept from
                    // the prepaint that precedes the click, so the preview
                    // knows where to grow from.
                    let placed = Rc::new(Cell::new(Bounds::default()));

                    div()
                        // The measuring child is positioned absolutely, and
                        // an absolute child measures its nearest positioned
                        // ancestor; without this it would report the row.
                        .relative()
                        .size(px(TRANSCRIPT_THUMBNAIL))
                        .flex_none()
                        .rounded(UI_RADIUS)
                        .overflow_hidden()
                        .border_1()
                        .border_color(cx.theme().border)
                        // Unique across rows: a row carries at most
                        // `MAX_ATTACHMENTS` images, so its band cannot overlap
                        // the next row's.
                        .id(("entry-image", index * MAX_ATTACHMENTS + position))
                        // A thumbnail is cropped to a square and small enough
                        // to only recognize the image by, so opening it is the
                        // only way to read what was sent.
                        .cursor_pointer()
                        .aria_label(t!("agent-transcript-image-open"))
                        .on_prepaint({
                            let placed = placed.clone();

                            move |bounds, _, _| placed.set(bounds)
                        })
                        .on_click(cx.listener({
                            let image = image.clone();

                            move |this, _, _, cx| {
                                this.zoom_image(image.clone(), Some(placed.get()), cx)
                            }
                        }))
                        .child(img(image.clone()).size_full().object_fit(ObjectFit::Cover))
                }))
                .into_any_element(),
        )
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

        let (icon, heading, reason, status, detail) =
            match &conversation.content.entries()[index].item {
                SessionItem::CommandExecution {
                    purpose,
                    aggregated_output,
                    status,
                    exit_code,
                    ..
                } => {
                    // Belt and braces: a non-zero exit code is a failure even if
                    // the provider reported the execution as completed.
                    let state = status.as_deref().unwrap_or("inProgress");

                    let failed = matches!(state, "failed" | "declined")
                        || exit_code.is_some_and(|code| code != 0);

                    let state = if failed { "failed" } else { state };

                    let detail = aggregated_output.as_deref().unwrap_or("");

                    (
                        IconName::SquareTerminal,
                        command_execution_heading(purpose.as_deref()).to_string(),
                        failed
                            .then(|| command_failure_reason(aggregated_output.as_deref()))
                            .flatten(),
                        Some(state.to_string()),
                        Some(detail),
                    )
                }

                SessionItem::FileChange {
                    paths,
                    diff,
                    status,
                    ..
                } => (
                    IconName::File,
                    t!("agent-transcript-edit-paths", paths = paths).into_owned(),
                    None,
                    Some(status.as_deref().unwrap_or("inProgress").to_string()),
                    diff.as_deref().filter(|diff| !diff.trim().is_empty()),
                ),

                SessionItem::Other {
                    kind,
                    title,
                    output,
                    status,
                    ..
                } => (
                    if kind == "webSearch" {
                        IconName::Globe
                    } else {
                        IconName::Settings2
                    },
                    if title.trim().is_empty() {
                        kind.clone()
                    } else {
                        format!("{kind} {title}")
                    },
                    None,
                    Some(status.as_deref().unwrap_or("inProgress").to_string()),
                    output.as_deref().filter(|output| !output.trim().is_empty()),
                ),

                SessionItem::Reasoning { summary, .. } => (
                    IconName::Bot,
                    t!("agent-transcript-thinking").to_string(),
                    None,
                    None,
                    summary.as_deref().filter(|text| !text.trim().is_empty()),
                ),

                _ => return div().into_any_element(),
            };

        let expandable = detail.is_some();
        let expanded = expandable && self.disclosures.row_expanded(index);

        let detail_reveal = self
            .disclosures
            .progress(RevealKey::Row(index), Instant::now());

        let detail_part = RevealedPart::Block(RevealKey::Row(index));
        let detail_height = self.disclosures.height(detail_part);
        let detail_view = cx.entity().downgrade();

        let status_label = match status.as_deref() {
            Some("failed") => t!("agent-transcript-status-failed"),
            Some("declined") => t!("agent-transcript-status-declined"),
            Some("completed") => t!("agent-transcript-status-completed"),
            Some("inProgress") => t!("agent-transcript-status-in-progress"),
            Some(status) => status.into(),
            None => t!("agent-transcript-no-status"),
        };

        // The outcome is a mark rather than a word: it lands in the same slot
        // on every card, so a run of steps can be scanned down that column
        // instead of read. The wording stays in the row's accessible label.
        let tone = match status.as_deref() {
            Some("failed" | "declined") => AgentCardTone::Failed,
            _ => AgentCardTone::Neutral,
        };

        let status_icon = status.as_deref().map(|state| match state {
            "failed" | "declined" => (IconName::CircleX, cx.theme().danger),
            "completed" => (IconName::Check, cx.theme().success),
            _ => (IconName::Minus, cx.theme().muted_foreground),
        });

        let accessible_label = format!(
            "{}. {}{}",
            heading,
            status_label,
            if expandable {
                if self.disclosures.is_disclosing(RevealKey::Row(index)) {
                    t!("agent-transcript-accessibility-expanded")
                } else {
                    t!("agent-transcript-accessibility-collapsed")
                }
            } else {
                "".into()
            }
        );

        // A failure reason shows whether or not the step is expanded, so a
        // failed row usually heads a block even while its output is hidden.
        // Otherwise the header heads a block for exactly as long as there is
        // one: it squares off with the detail's arrival and returns to a pill
        // the moment the detail has finished shrinking away.
        let heads_body = reason.is_some() || (expanded && detail_reveal > 0.0);

        let mut header = AgentDisclosureRow::new(("wl-head", index), heading)
            .type_icon(icon)
            .tone(tone)
            .heads_body(heads_body)
            .accessible_label(accessible_label);

        if let Some((icon, color)) = status_icon {
            header = header.status(icon, color);
        }

        if expandable {
            header = header.expanded(expanded).opening(detail_reveal);
        }

        let header =
            header.render(cx).when(expandable, |this| {
                this.on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_disclosure(RevealKey::Row(index), cx)
                }))
            });

        // The block under the header carries the header's own fill, so a
        // failed step reads as one tinted shape rather than as a tinted
        // heading with untinted output hanging off it.
        let card_body = heads_body.then(|| {
            div()
                .w_full()
                .bg(tone.colors(cx).background)
                .rounded_b(px(AGENT_CARD_RADIUS))
                // Why a step failed belongs on the card rather than behind the
                // disclosure: it is what the reader decides their next move from,
                // and the full transcript below it is usually a stack trace.
                .children(reason.map(|reason| {
                    div()
                        .w_full()
                        .pl(px(AGENT_DISCLOSURE_DETAIL_INSET))
                        .pr(px(AGENT_CARD_PADDING_X))
                        .pb(px(AGENT_CARD_BODY_PADDING_Y))
                        .text_size(px(AGENT_CARD_DETAIL_SIZE))
                        .text_color(cx.theme().danger.opacity(0.85))
                        .child(reason)
                }))
                .children(detail.filter(|_| expanded).map(|detail| {
                    let body = if is_code_item(
                        &self.conversation.borrow().content.entries()[index].item,
                    ) {
                        let view = self.code_transcripts.ensure(
                            index,
                            &self.conversation.borrow().content.entries()[index].item,
                            cx,
                        );

                        div()
                            .w_full()
                            .modern_context_menu(Self::copy_menu(cx.entity().downgrade(), index))
                            .child(view)
                            .into_any_element()
                    } else {
                        let detail_scroll = window
                            .use_keyed_state(("wl-scroll", index), cx, |_, _| {
                                ScrollHandle::default()
                            })
                            .read(cx)
                            .clone();

                        div()
                            .w_full()
                            .relative()
                            .child(
                                div()
                                    .id(("wl-out", index))
                                    .w_full()
                                    .max_h(px(256.))
                                    .overflow_y_scroll()
                                    .track_scroll(&detail_scroll)
                                    .occlude()
                                    .modern_context_menu(Self::copy_menu(
                                        cx.entity().downgrade(),
                                        index,
                                    ))
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        markdown_view(
                                            ("wl-md", index),
                                            detail.to_owned(),
                                            cwd.clone(),
                                        )
                                        .style(transcript_text_style(cx))
                                        .selectable(true),
                                    ),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .right_0()
                                    .bottom_0()
                                    .w(px(16.0))
                                    .child(
                                        Scrollbar::vertical(&detail_scroll)
                                            .id(("wl-scrollbar", index)),
                                    ),
                            )
                            .into_any_element()
                    };

                    let block = div()
                        // Expanded content takes the card's own inset on both
                        // sides. The rule above it already says the detail belongs
                        // to the header, so indenting it as well would spend a
                        // third of a narrow card on saying it twice — and command
                        // output is exactly the content that needs the width.
                        .w_full()
                        .border_t_1()
                        .border_color(cx.theme().border.opacity(0.6))
                        .px(px(AGENT_CARD_PADDING_X))
                        .py(px(AGENT_CARD_BODY_PADDING_Y))
                        .child(body);

                    revealed_block(
                        block,
                        detail_part,
                        detail_reveal,
                        detail_height,
                        px(0.),
                        detail_view,
                    )
                }))
        });

        agent_card()
            .id(("entry", index))
            .modern_context_menu(Self::copy_menu(cx.entity().downgrade(), index))
            .child(header)
            .children(card_body)
            .into_any_element()
    }

    fn render_question_message(
        &self,
        index: usize,
        id: String,
        text: String,
        questions: Vec<Question>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let owner = self.owner().cloned();

        v_flex()
            .w_full()
            .gap_2()
            .p_3()
            .rounded(UI_RADIUS)
            .border_1()
            .border_color(cx.theme().border)
            .child(self.render_agent_row(index, text, cx))
            .child(div().children(owner.map(|owner| {
                Button::new(("message-questions", index))
                    .ghost()
                    .small()
                    .label(t!("agent-question-open"))
                    .on_click(move |_, _, cx| {
                        let _ = owner.update(cx, |pane, cx| {
                            pane.open_message_questions(&id, questions.clone(), cx)
                        });
                    })
            })))
            .into_any_element()
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

            self.turn_specs(turn, start, end, collapse, &mut specs);
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
        let parts = self.revealed_parts(key);

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

    /// The list rows a run toggle or a turn fold currently has on screen,
    /// whichever ramp they happen to be travelling on this frame.
    fn revealed_parts(&self, key: RevealKey) -> Vec<RevealedPart> {
        (0..self.rows.len())
            .filter(|ix| match key {
                RevealKey::Group(run_start) => self.run_over(*ix) == Some(run_start),
                RevealKey::Turn(turn) => self.fold_over(*ix) == Some(turn),
                RevealKey::Row(_) | RevealKey::Annotation(_) => false,
            })
            .filter_map(|ix| revealed_part(&self.rows[ix].spec))
            .collect()
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

    /// The disclosure whose ramp this list row travels on this frame.
    ///
    /// Rows that were already there report `None` and render at rest. A step
    /// of a run inside an unfolded turn is on screen by two disclosures at
    /// once; it follows the fold while the fold is moving, because the fold is
    /// then moving everything under it, and its run the rest of the time, so
    /// a run opened inside a resting turn still travels.
    pub(crate) fn revealed_by(&self, ix: usize, now: Instant) -> Option<RevealKey> {
        let fold = self.fold_over(ix).map(RevealKey::Turn);
        let run = self.run_over(ix).map(RevealKey::Group);

        match (fold, run) {
            (Some(fold), Some(run)) if !self.disclosures.moving(fold, now) => Some(run),
            (Some(fold), _) => Some(fold),
            (None, run) => run,
        }
    }

    /// The run whose expanded toggle put this row on screen. A run's steps
    /// follow its toggle contiguously, so walking back over them to the
    /// toggle is what identifies the run without the row specs having to
    /// carry it.
    fn run_over(&self, ix: usize) -> Option<usize> {
        if !matches!(self.rows.get(ix)?.spec, RowSpec::Work { .. }) {
            return None;
        }

        for cursor in (0..ix).rev() {
            match self.rows[cursor].spec {
                RowSpec::Work { .. } => continue,

                RowSpec::RunToggle {
                    run_start,
                    expanded: true,
                    ..
                } => return Some(run_start),

                _ => break,
            }
        }

        None
    }

    /// The turn whose unfolded "Show work" row put this row on screen.
    ///
    /// The fold heads its turn, and every row of the turn below it that a
    /// folded turn would not show is the fold's. Rows a folded turn keeps —
    /// the final reply, an error, a steered prompt — sit among them and are
    /// walked over, so the work after a steered prompt still finds its fold.
    /// Only a settled turn has one, which spares an unsettled conversation
    /// the walk.
    fn fold_over(&self, ix: usize) -> Option<u64> {
        let turn = self.row_turn(ix)?;

        if !self.conversation.borrow().turns.is_settled(turn) || !self.hidden_by_fold(ix) {
            return None;
        }

        for cursor in (0..ix).rev() {
            match self.rows[cursor].spec {
                RowSpec::TurnFold {
                    turn: heads,
                    folded: false,
                    ..
                } if heads == turn => return Some(turn),

                _ if self.row_turn(cursor) == Some(turn) => continue,
                _ => break,
            }
        }

        None
    }

    /// Whether this row is one a folded turn would take off the screen.
    fn hidden_by_fold(&self, ix: usize) -> bool {
        match self.rows[ix].spec {
            RowSpec::Work { .. } | RowSpec::RunToggle { .. } => true,
            RowSpec::Entry { index, .. } => !self.survives_fold(index),
            _ => false,
        }
    }

    /// The turn a list row belongs to, for the rows that belong to one.
    fn row_turn(&self, ix: usize) -> Option<u64> {
        match self.rows.get(ix)?.spec {
            RowSpec::Entry { index, .. } | RowSpec::Work { index, .. } => {
                Some(self.conversation.borrow().content.entries()[index].turn)
            }

            RowSpec::RunToggle { run_start, .. } => {
                Some(self.conversation.borrow().content.entries()[run_start].turn)
            }

            RowSpec::TurnFold { turn, .. } | RowSpec::Interrupted { turn, .. } => Some(turn),
            RowSpec::TurnSummary { .. } | RowSpec::Working { .. } => None,
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

    /// Render one turn: the opening prompt, the work disclosure and the rows
    /// it hides, the final reply, and last the "Worked for Ns" summary.
    /// Running turns render chronologically.
    pub(crate) fn entry_spec(&self, index: usize) -> RowSpec {
        let fingerprint = entry_fingerprint(
            &self.conversation.borrow().content.entries()[index].item,
            self.disclosures.row_expanded(index),
            self.disclosures.annotation_expanded(index),
        );

        // A reply being typed lays out to the part let through so far, so its
        // signature follows that edge rather than the text behind it. The
        // edge sits above the length bits, which keeps every position of it
        // distinct from every length the text could have.
        let fingerprint = match self.typed_edge(index) {
            Some(shown) => fingerprint ^ ((shown as u64) << 32),
            None => fingerprint,
        };

        RowSpec::Entry { index, fingerprint }
    }

    pub(crate) fn work_spec(&self, index: usize) -> RowSpec {
        RowSpec::Work {
            index,
            fingerprint: entry_fingerprint(
                &self.conversation.borrow().content.entries()[index].item,
                self.disclosures.row_expanded(index),
                false,
            ),
        }
    }

    /// Data-only description of every transcript row, in render order. This
    /// is the single source of truth for the transcript's structure; the
    /// virtualized list builds elements only for the visible slice of it.
    #[cfg(test)]
    pub(crate) fn build_row_specs(&self, collapse: CollapseRows) -> Vec<RowSpec> {
        let mut rows = Vec::new();
        let mut start = 0;
        let shared = self.conversation.clone();
        let conversation = shared.borrow();
        let items = conversation.content.entries();

        while start < items.len() {
            let turn = items[start].turn;
            let mut end = start + 1;

            while end < items.len() && items[end].turn == turn {
                end += 1;
            }

            self.turn_specs(turn, start, end, collapse, &mut rows);
            start = end;
        }

        // Live progress row, pinned below everything the running turn has
        // produced; replaced by the turn's fold header on completion.
        if self.conversation.borrow().live.is_working() {
            rows.push(RowSpec::Working {
                compacting: self.conversation.borrow().live.is_compacting(),
            });
        }

        rows
    }

    pub(crate) fn turn_specs(
        &self,
        turn: u64,
        start: usize,
        end: usize,
        collapse: CollapseRows,
        rows: &mut Vec<RowSpec>,
    ) {
        // How the turn closes, once it has one. A stopped turn is closed by its
        // own marker; otherwise an elapsed-time line, when the session reported
        // a duration at all.
        let summary = turn_summary(
            self.conversation.borrow().turns.was_interrupted(turn),
            self.conversation.borrow().turns.seconds(turn),
        );

        if summary == Some(TurnSummary::Interrupted) {
            self.stream_specs(start, end, &|_| false, collapse, rows);

            rows.push(RowSpec::Interrupted {
                turn,
                output_tokens: self.conversation.borrow().turns.output_tokens(turn),
            });

            return;
        }

        // Running (or pre-thread) turn: plain chronological stream, because its
        // work is what the user is watching happen. Folding keys off the turn
        // having settled rather than off a known duration, so a replayed turn
        // folds too — the transcript file carries no timing for it.
        if !self.conversation.borrow().turns.is_settled(turn) {
            self.stream_specs(start, end, &|_| false, collapse, rows);

            return;
        }

        // Only the mode that names work folds a settled turn's work away by
        // default, and only the modes that offer the disclosure can fold at
        // all. "Only tool calls" reads the work inline, so it carries no
        // disclosure and no per-turn toggle; the other two keep the control
        // and record hand-folds against whichever direction their default
        // points.
        let discloses_work = !matches!(collapse, CollapseRows::ToolCalls);
        let folded = discloses_work && folds_turns(collapse) != self.disclosures.turn_toggled(turn);

        // Only the prompt that opened the turn heads it. A message steered
        // into a turn already in flight was written after part of the reply
        // existed, so hoisting it here would show it above output it never
        // saw; it keeps its place in the stream instead.
        let shared = self.conversation.clone();
        let conversation = shared.borrow();
        let items = conversation.content.entries();

        let opening_user =
            (start..end).find(|&i| matches!(&items[i].item, SessionItem::UserMessage { .. }));

        if let Some(i) = opening_user {
            rows.push(self.entry_spec(i));
        }

        // What the fold owns: everything the expanded turn shows that the
        // folded one does not. Counting it here keeps the disclosure's label
        // honest and lets a turn with nothing to hide skip the control.
        let shown = |i: usize| !hidden(&items[i].item) && Some(i) != opening_user;

        let row_count = (start..end)
            .filter(|&i| shown(i) && !self.survives_fold(i))
            .count();

        // Above the rows it discloses, so expanding inserts them below the
        // control the user just clicked instead of further up the turn.
        if discloses_work && row_count > 0 {
            rows.push(RowSpec::TurnFold {
                turn,
                row_count,
                folded,
            });
        }

        if folded {
            for i in (start..end).filter(|&i| shown(i) && self.survives_fold(i)) {
                rows.push(self.entry_spec(i));
            }
        } else {
            let skip = |i: usize| Some(i) == opening_user;

            self.stream_specs(start, end, &skip, collapse, rows);
        }

        // The turn's summary closes it, below the answer it accounts for, the
        // same place the interrupted marker sits. A replayed turn reaches here
        // with no duration to state and simply ends after its reply.
        if let Some(TurnSummary::Worked(seconds)) = summary {
            rows.push(RowSpec::TurnSummary {
                seconds,
                output_tokens: self.conversation.borrow().turns.output_tokens(turn),
            });
        }
    }

    /// Chronological rows for a slice of the transcript, collapsing runs of
    /// consecutive work-log rows into a "+N tool calls" toggle (unless the
    /// collapse setting is off). Hidden entries are transparent: they neither
    /// render nor split a run.
    pub(crate) fn stream_specs(
        &self,
        start: usize,
        end: usize,
        skip: &dyn Fn(usize) -> bool,
        collapse: CollapseRows,
        rows: &mut Vec<RowSpec>,
    ) {
        let mut i = start;
        let shared = self.conversation.clone();
        let conversation = shared.borrow();
        let items = conversation.content.entries();

        while i < end {
            let item = &items[i].item;

            if skip(i) || hidden(item) {
                i += 1;

                continue;
            }

            if !is_work_row(item) {
                rows.push(self.entry_spec(i));
                i += 1;

                continue;
            }

            // Extend the run across consecutive (possibly hidden) work rows.
            let run_start = i;
            let mut visible: Vec<usize> = Vec::new();
            let mut j = i;

            while j < end && !skip(j) && (hidden(&items[j].item) || is_work_row(&items[j].item)) {
                if !hidden(&items[j].item) {
                    visible.push(j);
                }

                j += 1;
            }

            if !matches!(collapse, CollapseRows::Off) && visible.len() > 1 {
                let expanded = self.disclosures.group_expanded(run_start);

                rows.push(RowSpec::RunToggle {
                    run_start,
                    tool_count: visible.len(),
                    expanded,
                });

                if expanded {
                    for &k in &visible {
                        rows.push(self.work_spec(k));
                    }
                }
            } else {
                for &k in &visible {
                    rows.push(self.work_spec(k));
                }
            }

            i = j;
        }
    }

    /// Whether an entry stays on screen while its turn is folded. Errors,
    /// compaction boundaries and steered prompts do: an error is what the
    /// user needs to act on, a boundary marks where the conversation above
    /// it stopped being verbatim, and words the user typed are never work to
    /// hide. The final reply does too, selected by identity rather than
    /// moved, because visible events can still arrive after it while the
    /// turn closes; everything between the prompt and that answer is what the
    /// fold hides.
    pub(crate) fn survives_fold(&self, index: usize) -> bool {
        let shared = self.conversation.clone();
        let conversation = shared.borrow();
        let items = conversation.content.entries();
        let entry = &items[index];

        match &entry.item {
            SessionItem::Error { .. }
            | SessionItem::Compaction { .. }
            | SessionItem::UserMessage { .. } => true,

            SessionItem::AgentMessage {
                questions: Some(_), ..
            } => true,

            SessionItem::AgentMessage { .. } => {
                !hidden(&entry.item)
                    && items[index + 1..]
                        .iter()
                        .take_while(|later| later.turn == entry.turn)
                        .all(|later| {
                            hidden(&later.item)
                                || !matches!(later.item, SessionItem::AgentMessage { .. })
                        })
            }

            _ => false,
        }
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
        let new_mid = new.len() - suffix - prefix;

        if old_mid.len() == new_mid {
            self.transcript_list.remeasure_items(old_mid);
        } else {
            self.transcript_list.splice(old_mid, new_mid);
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

// The streamed reply, let onto the screen a character at a time.
//
// A backend delivers a reply a few words at a time, and each chunk landing
// whole makes the text jump in by fits and starts. The typewriter holds a
// moving edge behind what has arrived and lets the reply through it one
// character at a time, closing on the streamed text fast enough that the
// edge never trails it by more than a fraction of a second.

/// How quickly the edge closes on what has arrived: the backlog behind it
/// shrinks by a factor of e over this span. A chunk of forty characters is
/// most of the way on screen a quarter of a second after landing, so the reply
/// reads as being typed rather than as being held back, and once the stream
/// stops the last chunk finishes within a beat.
const CATCH_UP: Duration = Duration::from_millis(125);

/// The slowest the edge moves while anything waits behind it, in characters
/// per second. Closing by a share alone would let the final characters of a
/// chunk trickle in over many frames, which reads as the reply stalling on
/// its last word; the floor keeps that tail arriving at a steady typing pace.
const FLOOR_RATE: f32 = 60.0;

/// The edge of a reply being let onto the screen, and the entry it crosses.
///
/// One reply is typed at a time. A new reply starting while an older one
/// still has text waiting lets the older one land whole: the reader's eye
/// has already moved on to where the new text appears.
struct Typewriter {
    index: usize,

    /// Characters shown, carrying the fraction between frames so a rate
    /// below one character a frame still moves.
    shown: f32,

    ticked: Instant,
}

impl Typewriter {
    /// Start typing entry `index` from `shown` characters, which is what the
    /// entry already had on screen before streamed text reached it.
    fn start(index: usize, shown: usize, now: Instant) -> Self {
        Self {
            index,
            shown: shown as f32,
            ticked: now,
        }
    }

    fn index(&self) -> usize {
        self.index
    }

    /// Move the edge towards `total`, the reply's length in characters as of
    /// now. Returns whether anything is still waiting behind it.
    ///
    /// The step is the larger of the catch-up share and the floor, so a big
    /// chunk closes quickly while a lone character still arrives promptly.
    /// A long gap between frames — a pane that was hidden — closes the whole
    /// backlog at once rather than typing text the reader was not watching.
    fn advance(&mut self, total: usize, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.ticked).as_secs_f32();

        self.ticked = now;

        let total = total as f32;
        let backlog = (total - self.shown).max(0.0);
        let share = 1.0 - (-elapsed / CATCH_UP.as_secs_f32()).exp();
        let step = (backlog * share).max(FLOOR_RATE * elapsed).min(backlog);

        self.shown = (self.shown + step).min(total);

        self.shown < total
    }

    /// How many characters of the reply are on screen.
    fn shown(&self) -> usize {
        self.shown as usize
    }
}

/// The part of `text` an edge `chars` characters in lets through. Counted in
/// characters rather than bytes so a CJK reply types at the same pace as a
/// latin one and the cut never lands inside a character.
fn shown_prefix(text: &str, chars: usize) -> &str {
    text.char_indices()
        .nth(chars)
        .map_or(text, |(end, _)| &text[..end])
}
