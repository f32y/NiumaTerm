use std::collections::{HashMap, HashSet};
use std::time::Instant;

use chrono::{DateTime, Local};
use gpui::prelude::*;
use gpui::{
    Context, FollowMode, IntoElement, ListAlignment, ListState, Pixels, Render, SharedString,
    Window, div, list, px, relative,
};
use gpui_component::button::Button;
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme as _, ElementExt as _, IconName, Sizable as _};
use nmt_agent_utils::chat::{Item as SessionItem, ReplayTurn};
use nmt_config::agent::CollapseRows;
use nmt_i18n::i18n;

use crate::AgentPane;
use crate::composer::PALETTE_MAX_HEIGHT;
use crate::profile::AgentKind;
use crate::settings::{AgentSettings, UI_RADIUS};
use crate::transcript::render::TRANSCRIPT_LINE_HEIGHT;
use crate::transcript::reveal::Disclosures;
use crate::transcript::rows::TranscriptRow;
use crate::transcript::{Entry, ReadingPosition, VirtualTranscriptState, is_work_row};

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
    pub(crate) items: Vec<Entry>,
    /// Virtualized transcript: only visible rows build elements each frame.
    /// `rows` mirrors the list's item count; render() diffs freshly built
    /// rows against it and splices/remeasures just the changed range.
    pub(crate) transcript_list: ListState,
    pub(crate) rows: Vec<TranscriptRow>,
    /// Row heights depend on the prose and technical-content fonts, which the
    /// specs can't see; the last-seen values trigger a full remeasure on change.
    transcript_font: (SharedString, f32, SharedString, f32),
    /// Virtual rows cache measured heights; a width change can rewrap prose
    /// without changing row fingerprints, so the viewport width is tracked too.
    transcript_width: Option<Pixels>,
    /// Last measured viewport height, which is how much empty space below the
    /// conversation lets its final row reach the top of the screen.
    transcript_height: Option<Pixels>,
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
    /// Settled turns the user has flipped away from what the collapse setting
    /// does by default: unfolded where it folds a turn's work behind the
    /// "Show work" disclosure, folded where it leaves the work on screen.
    pub(crate) toggled_turns: HashSet<u64>,
    /// Collapse setting the rows above were built under, so a change to it can
    /// retire the per-turn and per-run departures from the mode it replaces.
    collapse_mode: CollapseRows,
    /// Which parts of the transcript are open, how far through their motion
    /// they are, and how tall each one lays out to.
    pub(crate) disclosures: Disclosures,
    /// Long expanded code transcripts retain their segmented source and
    /// independent uniform-list position while visible. Collapsing a row drops
    /// the duplicate source so large outputs do not stay resident twice.
    pub(crate) virtual_transcripts: HashMap<usize, VirtualTranscriptState>,
    /// Turns that have finished, whether in this process or in a session this
    /// view replayed. Folding keys off this rather than off a known duration,
    /// because a resumed conversation carries no timing: the CLI reports a
    /// turn's wall time only on the live stream, never in the transcript file.
    pub(crate) settled_turns: HashSet<u64>,
    /// Settled turn durations drive the closing summary without masquerading
    /// as provider transcript items. Absent for a replayed turn.
    pub(crate) completed_turn_seconds: HashMap<u64, u64>,
    /// Final provider-reported output tokens for settled turns, keyed by the
    /// same local turn id as the duration used by the fold header.
    pub(crate) completed_turn_output_tokens: HashMap<u64, u64>,
    /// User-stopped turns use a status row instead of an elapsed-time
    /// disclosure, whether or not provider activity was already visible.
    pub(crate) interrupted_turns: HashSet<u64>,
    /// Start time of the running turn. While set, a ticking "Working for Ns"
    /// row renders at the transcript end.
    pub(crate) working_started: Option<Instant>,
    /// Output tokens reported for the running turn.
    pub(crate) working_output_tokens: Option<u64>,
    /// What the backend says the running turn is currently doing, when that is
    /// something the elapsed time and token count do not show. A turn waiting
    /// out a provider retry looks identical to one thinking slowly, and the two
    /// call for opposite reactions from the user.
    pub(crate) working_detail: Option<String>,
    /// The backend is rewriting the conversation to reclaim context. Turn
    /// output pauses for the duration, so the live progress row explains the
    /// wait instead of leaving a bare seconds counter that looks stalled.
    pub(crate) compacting: bool,
    /// Presentation inputs rather than owned state: the working directory
    /// resolves transcript links, and the provider decides a few labels.
    pub(crate) cwd: Option<String>,
    pub(crate) kind: AgentKind,
    /// Revision of the conversation this view was last filled from, for a view
    /// that mirrors content someone else owns rather than accumulating its own.
    source_revision: Option<u64>,
    /// The pane whose conversation this is, for the row actions that address
    /// the conversation rather than the row: branching in front of a prompt,
    /// rewinding to one. Absent on a view that mirrors somebody else's
    /// conversation — a child agent's or a workflow member's — where those
    /// actions have no conversation of this pane's to act on.
    owner: Option<gpui::WeakEntity<AgentPane>>,
}

impl TranscriptView {
    pub fn new(kind: AgentKind, cwd: Option<String>) -> Self {
        Self {
            items: Vec::new(),
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
            disclosures: Disclosures::default(),
            toggled_turns: HashSet::new(),
            collapse_mode: CollapseRows::default(),
            virtual_transcripts: HashMap::new(),
            settled_turns: HashSet::new(),
            completed_turn_seconds: HashMap::new(),
            completed_turn_output_tokens: HashMap::new(),
            interrupted_turns: HashSet::new(),
            working_started: None,
            working_output_tokens: None,
            working_detail: None,
            compacting: false,
            cwd,
            kind,
            source_revision: None,
            owner: None,
        }
    }

    /// Claim this view as one pane's own conversation, which is what makes its
    /// rows offer the actions that address the conversation.
    pub(crate) fn set_owner(&mut self, owner: gpui::WeakEntity<AgentPane>) {
        self.owner = Some(owner);
    }

    pub(crate) fn owner(&self) -> Option<&gpui::WeakEntity<AgentPane>> {
        self.owner.as_ref()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Mirror a conversation this view does not own. `revision` identifies the
    /// source's current content, so an unchanged source costs one comparison
    /// rather than a rebuild. Row indices stay stable because the source only
    /// appends and merges in place, which keeps expansion state valid.
    pub fn show_items(&mut self, items: &[SessionItem], revision: u64, cx: &mut Context<Self>) {
        if self.source_revision == Some(revision) {
            return;
        }
        let follow = self.source_revision.is_none();
        self.source_revision = Some(revision);
        self.items = items
            .iter()
            .map(|item| Entry {
                at: String::new(),
                turn: 0,
                item: item.clone(),
                images: Vec::new(),
            })
            .collect();
        if follow {
            self.scroll_to_bottom();
        }
        cx.notify();
    }

    /// Drop the conversation and every piece of view state derived from it.
    /// Used when the owning view switches to a different conversation, so one
    /// conversation's expansion and scroll position cannot leak into another's.
    pub(crate) fn clear(&mut self) {
        self.items.clear();
        self.stashed_position = None;
        self.reserve_below = false;
        self.scroll_to_bottom();
        self.toggled_turns.clear();
        self.disclosures.clear();
        self.settled_turns.clear();
        self.completed_turn_seconds.clear();
        self.completed_turn_output_tokens.clear();
        self.interrupted_turns.clear();
        self.virtual_transcripts.clear();
        self.working_started = None;
        self.working_output_tokens = None;
        self.working_detail = None;
        self.compacting = false;
    }

    /// Append one turn of a restored conversation under its own turn id, with
    /// the accounting the provider persisted for it. Replaying it settles the
    /// turn, so it folds its work exactly like one completed in this process.
    /// Its duration is a separate question: the transcript file records none,
    /// so a replayed turn usually closes without an elapsed-time line rather
    /// than stating a time the session never reported.
    pub(crate) fn append_replay(&mut self, turn: u64, replay: ReplayTurn, cx: &mut Context<Self>) {
        for entry in replay.items {
            self.items.push(Entry {
                at: entry
                    .at
                    .and_then(|at| DateTime::from_timestamp(at, 0))
                    .map(|at| at.with_timezone(&Local).format("%H:%M").to_string())
                    .unwrap_or_default(),
                turn,
                item: entry.item,
                images: Vec::new(),
            });
        }

        self.settled_turns.insert(turn);

        if replay.interrupted {
            self.interrupted_turns.insert(turn);
        }
        if let Some(seconds) = replay.seconds {
            self.completed_turn_seconds.insert(turn, seconds);
        }
        if let Some(output_tokens) = replay.output_tokens {
            self.completed_turn_output_tokens
                .insert(turn, output_tokens);
        }
        cx.notify();
    }

    /// Append one entry with an explicit stamp, for content this view records
    /// outside the normal push path.
    pub(crate) fn push_stamped(&mut self, turn: u64, item: SessionItem) {
        self.items.push(Entry {
            at: Local::now().format("%H:%M").to_string(),
            turn,
            item,
            images: Vec::new(),
        });
    }

    pub(crate) fn contains_item(&self, id: &str) -> bool {
        self.items.iter().any(|entry| entry.item.id() == Some(id))
    }

    /// Fold an authoritative completed payload into the entry that streamed it.
    pub(crate) fn merge_completed(&mut self, item: &SessionItem) {
        for entry in &mut self.items {
            if entry.item.merge_completed(item) {
                break;
            }
        }
    }

    /// Extend a streamed item's text. Returns whether the result is non-empty,
    /// which is what tells the caller the row became visible.
    pub(crate) fn append_delta(
        &mut self,
        item_id: &str,
        delta: &str,
        select: fn(&mut SessionItem) -> Option<&mut Option<String>>,
    ) -> bool {
        for entry in &mut self.items {
            if entry.item.id() == Some(item_id)
                && let Some(text) = select(&mut entry.item)
            {
                let text = text.get_or_insert_default();
                text.push_str(delta);
                return !text.trim().is_empty();
            }
        }

        false
    }

    /// Latest non-empty assistant reply of `turn`, for notification bodies.
    pub(crate) fn latest_agent_message(&self, turn: u64) -> Option<&str> {
        self.items.iter().rev().find_map(|entry| match &entry.item {
            SessionItem::AgentMessage {
                text: Some(text), ..
            } if entry.turn == turn && !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
    }

    /// How many actions `turn` has taken: the tool calls, file changes and
    /// thinking passes it logged. Conversation text is the turn talking rather
    /// than working, so it does not count.
    pub(crate) fn turn_steps(&self, turn: u64) -> usize {
        self.items
            .iter()
            .filter(|entry| entry.turn == turn && is_work_row(&entry.item))
            .count()
    }

    /// Completed and total entries of the task list the agent is working from.
    /// Only the newest list counts: a task list is republished in full whenever
    /// it changes, so the earlier ones describe states the agent has left.
    pub(crate) fn task_tally(&self) -> Option<(u32, u32)> {
        self.items
            .iter()
            .rev()
            .find_map(|entry| entry.item.task_tally())
    }

    pub(crate) fn is_working(&self) -> bool {
        self.working_started.is_some()
    }

    pub(crate) fn is_compacting(&self) -> bool {
        self.compacting
    }

    pub(crate) fn set_compacting(&mut self, compacting: bool, cx: &mut Context<Self>) {
        self.compacting = compacting;
        cx.notify();
    }

    pub(crate) fn was_interrupted(&self, turn: u64) -> bool {
        self.interrupted_turns.contains(&turn)
    }

    pub(crate) fn mark_interrupted(&mut self, turn: u64) {
        self.interrupted_turns.insert(turn);
    }

    pub(crate) fn start_working(&mut self, cx: &mut Context<Self>) {
        self.working_started = Some(Instant::now());
        self.working_output_tokens = None;
        // Whatever the last turn was doing has nothing to say about this one.
        self.working_detail = None;
        cx.notify();
    }

    /// Report what the running turn is doing, or clear it once the backend has
    /// nothing further to add.
    pub(crate) fn set_working_detail(&mut self, detail: Option<String>, cx: &mut Context<Self>) {
        if self.working_detail != detail {
            self.working_detail = detail;
            cx.notify();
        }
    }

    pub(crate) fn set_working_output_tokens(&mut self, output_tokens: u64, cx: &mut Context<Self>) {
        if self.working_started.is_some() {
            self.working_output_tokens = Some(output_tokens);
            cx.notify();
        }
    }

    /// Settle the running turn's duration and output usage for its status row.
    /// These are view state rather than provider transcript content, so they
    /// stay outside the shared item stream.
    pub(crate) fn settle_turn(&mut self, turn: u64, cx: &mut Context<Self>) {
        let output_tokens = self.working_output_tokens.take();
        self.working_detail = None;
        if let Some(started) = self.working_started.take() {
            self.settled_turns.insert(turn);
            if !self.interrupted_turns.contains(&turn) {
                self.completed_turn_seconds
                    .insert(turn, started.elapsed().as_secs());
            }
            if let Some(output_tokens) = output_tokens {
                self.completed_turn_output_tokens
                    .insert(turn, output_tokens);
            }
            cx.notify();
        }
    }

    /// Discard a turn that never produced visible output, so an immediate stop
    /// leaves no elapsed-time row behind for work that did not happen.
    pub(crate) fn discard_turn(&mut self, turn: u64, cx: &mut Context<Self>) {
        self.working_started = None;
        self.working_output_tokens = None;
        self.working_detail = None;
        self.settled_turns.remove(&turn);
        self.completed_turn_seconds.remove(&turn);
        self.completed_turn_output_tokens.remove(&turn);
        self.compacting = false;
        cx.notify();
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
            self.toggled_turns.clear();
            // Anything mid-exit goes with its own state: dropping the reveal
            // alone would strand the disclosure open with nothing left to
            // finish shutting it.
            for key in self.disclosures.closing() {
                self.take_down_disclosure(key);
            }
            self.disclosures.forget_groups();
        }
        self.transcript_list.set_smooth_wheel_enabled(smooth_wheel);

        // Transcript rows, one folded/expanded section per turn (entries are
        // tagged with a monotonic turn id, so turns are contiguous slices).
        // Only the visible slice becomes elements; the spec diff tells the
        // list which rows changed shape.
        let specs = self.build_row_specs(collapse);
        self.sync_transcript_list(specs);

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
                                        .label(i18n("agent-transcript-scroll-bottom"))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.scroll_to_bottom();
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
            })
    }
}
