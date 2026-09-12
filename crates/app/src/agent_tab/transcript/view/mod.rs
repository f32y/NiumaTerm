use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    Bounds, Context, FollowMode, Image, IntoElement, ListAlignment, ListState, Pixels, Point,
    Render, SharedString, Window, div, list, px, relative,
};
use gpui_component::button::Button;
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme as _, ElementExt as _, IconName, Sizable as _};
use nmt_agent::chat::Item as SessionItem;
use nmt_agent::transcript::conversation::ConversationState;
use nmt_config::agent::CollapseRows;
use nmt_profiling::transcript::{Operation, Probe};
use rust_i18n::t;

use crate::agent_tab::AgentPane;
use crate::agent_tab::composer::PALETTE_MAX_HEIGHT;
use crate::agent_tab::fade::Fade;
use crate::agent_tab::profile::AgentKind;
use crate::agent_tab::settings::{AgentSettings, UI_RADIUS};
use crate::agent_tab::transcript::incremental::RowCache;
use crate::agent_tab::transcript::render::TRANSCRIPT_LINE_HEIGHT;
use crate::agent_tab::transcript::render::image_preview::ZOOM_DURATION;
use crate::agent_tab::transcript::reveal::Disclosures;
use crate::agent_tab::transcript::rows::{TranscriptRow, folds_turns};
use crate::agent_tab::transcript::{CodeTranscriptCache, Entry, ReadingPosition};

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
    pub(in crate::agent_tab) conversation: Rc<RefCell<ConversationState>>,
    pub(in crate::agent_tab) image_previews: RefCell<HashMap<(usize, usize), Arc<Image>>>,
    pub(super) row_cache: RowCache,

    /// Virtualized transcript: only visible rows build elements each frame.
    /// `rows` mirrors the list's item count; render() rebuilds the changed
    /// turn suffix and splices/remeasures just the changed range.
    pub(in crate::agent_tab) transcript_list: ListState,

    pub(in crate::agent_tab) rows: Vec<TranscriptRow>,

    /// Row heights depend on the prose and technical-content fonts, which the
    /// specs can't see; the last-seen values trigger a full remeasure on change.
    transcript_font: (SharedString, f32, SharedString, f32),

    /// Virtual rows cache measured heights; a width change can rewrap prose
    /// without changing row fingerprints, so the viewport width is tracked too.
    pub(super) transcript_width: Option<Pixels>,

    /// Last measured viewport height, which is how much empty space below the
    /// conversation lets its final row reach the top of the screen.
    pub(super) transcript_height: Option<Pixels>,

    /// Where the viewport sits in the window, which is what turns the window
    /// bounds a thumbnail reports into a position inside the preview layer.
    pub(super) transcript_origin: Option<Point<Pixels>>,

    /// Reading position from before a picker started scrolling the transcript
    /// to the prompt it highlights, so cancelling that picker returns the
    /// conversation to where the user was reading it.
    pub(super) stashed_position: Option<ReadingPosition>,

    /// A picker is following the transcript, so empty space is left below the
    /// conversation. Without that room a prompt near the end cannot be lifted
    /// clear of the picker: the list stops scrolling once its last row is on
    /// screen, which leaves exactly those prompts behind the list naming
    /// them.
    pub(super) reserve_below: bool,

    /// Collapse setting the rows above were built under, so a change to it can
    /// retire the per-turn and per-run departures from the mode it replaces.
    collapse_mode: CollapseRows,

    /// Which parts of the transcript are open, how far through their motion
    /// they are, and how tall each one lays out to.
    pub(in crate::agent_tab) disclosures: Disclosures,

    /// Expanded technical output retains its parsed source and scroll position.
    /// Collapsing a row releases the extra source, syntax trees, and worker.
    pub(in crate::agent_tab) code_transcripts: CodeTranscriptCache,

    /// The reply being let onto the screen a character at a time, while one
    /// is. Only text that streams in through this view is typed: a restored
    /// or mirrored conversation arrives whole and is shown whole.
    typewriter: Option<Typewriter>,

    /// Presentation inputs rather than owned state: the working directory
    /// resolves transcript links, and the provider decides a few labels.
    pub(in crate::agent_tab) cwd: Option<String>,

    pub(in crate::agent_tab) kind: AgentKind,

    /// Revision of the conversation this view was last filled from, for a view
    /// that mirrors content someone else owns rather than accumulating its own.
    source_revision: Option<u64>,

    observed_version: (u64, u64),

    /// The image a reader opened at full size over the conversation. Held per
    /// conversation rather than per pane so a child agent's transcript
    /// enlarges its own images inside its own bounds. Stays through the
    /// layer's fade-out, which needs something to fade.
    pub(in crate::agent_tab) zoomed_image: Option<Arc<Image>>,

    /// Whether the preview layer is up or on its way out; the image alone
    /// cannot say, because it outlives the dismissal by the fade.
    pub(in crate::agent_tab) zoom_open: bool,

    pub(in crate::agent_tab) zoom_fade: Fade,

    /// The thumbnail the open image grew out of, in window coordinates, so
    /// the preview can shrink back into it. Absent when the image was opened
    /// from something with no place on screen, such as a link in the composer.
    pub(in crate::agent_tab) zoom_origin: Option<Bounds<Pixels>>,

    /// The pane whose conversation this is, for the row actions that address
    /// the conversation rather than the row: branching in front of a prompt,
    /// rewinding to one. Absent on a view that mirrors somebody else's
    /// conversation — a child agent's or a workflow member's — where those
    /// actions have no conversation of this pane's to act on.
    owner: Option<gpui::WeakEntity<AgentPane>>,
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
            stashed_position: None,
            reserve_below: false,
            transcript_font: Default::default(),
            transcript_width: None,
            transcript_height: None,
            transcript_origin: None,
            disclosures: Disclosures::new(folds_turns(collapse_mode)),
            collapse_mode,
            code_transcripts: CodeTranscriptCache::default(),
            typewriter: None,
            cwd,
            kind,
            source_revision: None,
            observed_version: (0, 0),
            zoomed_image: None,
            zoom_open: false,
            zoom_fade: Fade::lasting(ZOOM_DURATION),
            zoom_origin: None,
            owner: None,
        }
    }

    /// Claim this view as one pane's own conversation, which is what makes its
    /// rows offer the actions that address the conversation.
    pub(in crate::agent_tab) fn sync_content(&mut self) {
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

    pub(in crate::agent_tab) fn set_owner(&mut self, owner: gpui::WeakEntity<AgentPane>) {
        self.owner = Some(owner);
    }

    pub(in crate::agent_tab) fn owner(&self) -> Option<&gpui::WeakEntity<AgentPane>> {
        self.owner.as_ref()
    }

    pub(in crate::agent_tab) fn is_empty(&self) -> bool {
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

    pub(in crate::agent_tab) fn reset_presentation(&mut self) {
        self.image_previews.borrow_mut().clear();
        self.row_cache.invalidate(0);
        self.source_revision = None;
        self.stashed_position = None;
        self.reserve_below = false;
        self.scroll_to_bottom();
        self.disclosures.clear();
        self.code_transcripts.clear();
        self.typewriter = None;
    }

    /// The part of reply `index` the reader sees this frame: the whole of it
    /// unless its edge is still crossing the text.
    pub(in crate::agent_tab) fn shown_reply<'a>(&self, index: usize, text: &'a str) -> &'a str {
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
    pub(in crate::agent_tab) fn typed_edge(&self, index: usize) -> Option<usize> {
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
    pub(in crate::agent_tab) fn latest_agent_message(&self, turn: u64) -> Option<String> {
        self.conversation
            .borrow()
            .content
            .latest_agent_message(turn)
            .map(str::to_owned)
    }

    /// How many actions `turn` has taken: the tool calls, file changes and
    /// thinking passes it logged. Conversation text is the turn talking rather
    /// than working, so it does not count.
    pub(in crate::agent_tab) fn turn_steps(&self, turn: u64) -> usize {
        self.conversation.borrow().content.turn_steps(turn)
    }

    /// Completed and total entries of the task list the agent is working from.
    /// Only the newest list counts: a task list is republished in full whenever
    /// it changes, so the earlier ones describe states the agent has left.
    pub(in crate::agent_tab) fn task_tally(&self) -> Option<(u32, u32)> {
        self.conversation.borrow().content.task_tally()
    }

    pub(in crate::agent_tab) fn is_working(&self) -> bool {
        self.conversation.borrow().live.is_working()
    }

    pub(in crate::agent_tab) fn start_working(&mut self, cx: &mut Context<Self>) {
        if !self.conversation.borrow().live.is_working() {
            self.conversation.borrow_mut().start();
        }

        self.row_cache
            .invalidate(self.conversation.borrow().content.entries().len());

        cx.notify();
    }

    /// Discard a turn that never produced visible output, so an immediate stop
    /// leaves no elapsed-time row behind for work that did not happen.
    pub(in crate::agent_tab) fn discard_turn(&mut self, turn: u64, cx: &mut Context<Self>) {
        self.conversation.borrow_mut().live.discard();
        self.conversation.borrow_mut().turns.forget(turn);
        self.invalidate_turn_rows(turn);

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
        let collapse = settings.collapse_tool_calls;
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
        if self.collapse_mode != collapse {
            self.collapse_mode = collapse;

            // Anything mid-exit goes with its own state: dropping the reveal
            // alone would strand the disclosure open with nothing left to
            // finish shutting it.
            for key in self.disclosures.closing() {
                self.take_down_disclosure(key);
            }

            self.disclosures.forget_departures(folds_turns(collapse));
        }

        self.transcript_list.set_smooth_wheel_enabled(smooth_wheel);

        // Transcript rows, one folded/expanded section per turn (entries are
        // tagged with a monotonic turn id, so turns are contiguous slices).
        // Only the visible slice becomes elements; the spec diff tells the
        // list which rows changed shape.
        self.refresh_rows(collapse);

        if self.transcript_font != font {
            self.transcript_font = font;
            self.transcript_list.remeasure();
        }

        // The reserve is measured from the previous layout, which is the
        // viewport the next one will use unless the window is being resized.
        let reserve_below = self
            .reserve_below
            .then_some(self.transcript_height)
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
                                this.transcript_height = Some(bounds.size.height);
                                this.transcript_origin = Some(bounds.origin);

                                let width = bounds.size.width;

                                if this.transcript_width != Some(width) {
                                    this.transcript_width = Some(width);
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
            .children(self.render_zoomed_image(now, window, cx))
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

#[cfg(test)]
mod typewriter_tests;

#[cfg(test)]
mod fixtures;
