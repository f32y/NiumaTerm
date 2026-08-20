use nmt_i18n::i18n;

use crate::agent::transcript::is_work_row;
use crate::agent::*;

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
pub(crate) struct TranscriptView {
    pub(in crate::agent) items: Vec<Entry>,
    /// Virtualized transcript: only visible rows build elements each frame.
    /// `row_specs` mirrors the list's item count; render() diffs freshly built
    /// specs against it and splices/remeasures just the changed range.
    pub(in crate::agent) transcript_list: ListState,
    pub(in crate::agent) row_specs: Vec<RowSpec>,
    /// Row heights depend on the agent font, which the specs can't see; the
    /// last-seen font triggers a full remeasure when it changes.
    transcript_font: (SharedString, f64),
    /// Virtual rows cache measured heights; a width change can rewrap prose
    /// without changing row fingerprints, so the viewport width is tracked too.
    transcript_width: Option<Pixels>,
    /// Collapsed work-log runs the user has expanded, keyed by the index of
    /// the run's first transcript entry (stable — the list only appends).
    pub(in crate::agent) expanded_groups: HashSet<usize>,
    /// Completed turns the user has unfolded (completed turns fold their
    /// intermediate work rows behind a "Worked for Ns" header by default).
    pub(in crate::agent) expanded_turns: HashSet<u64>,
    /// Work-log rows whose detail (command output, reasoning text) is
    /// expanded, keyed by transcript index.
    pub(in crate::agent) expanded_rows: HashSet<usize>,
    /// Long expanded code transcripts retain their segmented source and
    /// independent uniform-list position while visible. Collapsing a row drops
    /// the duplicate source so large outputs do not stay resident twice.
    pub(in crate::agent) virtual_transcripts: HashMap<usize, VirtualTranscriptState>,
    /// Turns that have finished, whether in this process or in a session this
    /// view replayed. Folding keys off this rather than off a known duration,
    /// because a resumed conversation carries no timing: the CLI reports a
    /// turn's wall time only on the live stream, never in the transcript file.
    pub(in crate::agent) settled_turns: HashSet<u64>,
    /// Settled turn durations drive the closing summary without masquerading
    /// as provider transcript items. Absent for a replayed turn.
    pub(in crate::agent) completed_turn_seconds: HashMap<u64, u64>,
    /// Final provider-reported output tokens for settled turns, keyed by the
    /// same local turn id as the duration used by the fold header.
    pub(in crate::agent) completed_turn_output_tokens: HashMap<u64, u64>,
    /// User-stopped turns use a status row instead of an elapsed-time
    /// disclosure, whether or not provider activity was already visible.
    pub(in crate::agent) interrupted_turns: HashSet<u64>,
    /// Start time of the running turn. While set, a ticking "Working for Ns"
    /// row renders at the transcript end.
    pub(in crate::agent) working_started: Option<Instant>,
    /// Output tokens reported for the running turn.
    pub(in crate::agent) working_output_tokens: Option<u64>,
    /// What the backend says the running turn is currently doing, when that is
    /// something the elapsed time and token count do not show. A turn waiting
    /// out a provider retry looks identical to one thinking slowly, and the two
    /// call for opposite reactions from the user.
    pub(in crate::agent) working_detail: Option<String>,
    /// The backend is rewriting the conversation to reclaim context. Turn
    /// output pauses for the duration, so the live progress row explains the
    /// wait instead of leaving a bare seconds counter that looks stalled.
    pub(in crate::agent) compacting: bool,
    /// Presentation inputs rather than owned state: the working directory
    /// resolves transcript links, and the provider decides a few labels.
    pub(in crate::agent) cwd: Option<String>,
    pub(in crate::agent) kind: AgentKind,
    /// Revision of the conversation this view was last filled from, for a view
    /// that mirrors content someone else owns rather than accumulating its own.
    source_revision: Option<u64>,
}

impl TranscriptView {
    pub(crate) fn new(kind: AgentKind, cwd: Option<String>) -> Self {
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
            row_specs: Vec::new(),
            transcript_font: Default::default(),
            transcript_width: None,
            expanded_groups: HashSet::new(),
            expanded_turns: HashSet::new(),
            expanded_rows: HashSet::new(),
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
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Mirror a conversation this view does not own. `revision` identifies the
    /// source's current content, so an unchanged source costs one comparison
    /// rather than a rebuild. Row indices stay stable because the source only
    /// appends and merges in place, which keeps expansion state valid.
    pub(crate) fn show_items(
        &mut self,
        items: &[SessionItem],
        revision: u64,
        cx: &mut Context<Self>,
    ) {
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
        self.scroll_to_bottom();
        self.expanded_groups.clear();
        self.expanded_turns.clear();
        self.expanded_rows.clear();
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
    pub(in crate::agent) fn append_replay(
        &mut self,
        turn: u64,
        replay: ReplayTurn,
        cx: &mut Context<Self>,
    ) {
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
    pub(in crate::agent) fn push_stamped(&mut self, turn: u64, item: SessionItem) {
        self.items.push(Entry {
            at: Local::now().format("%H:%M").to_string(),
            turn,
            item,
            images: Vec::new(),
        });
    }

    pub(in crate::agent) fn contains_item(&self, id: &str) -> bool {
        self.items.iter().any(|entry| entry.item.id() == Some(id))
    }

    /// Fold an authoritative completed payload into the entry that streamed it.
    pub(in crate::agent) fn merge_completed(&mut self, item: &SessionItem) {
        for entry in &mut self.items {
            if entry.item.merge_completed(item) {
                break;
            }
        }
    }

    /// Extend a streamed item's text. Returns whether the result is non-empty,
    /// which is what tells the caller the row became visible.
    pub(in crate::agent) fn append_delta(
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
    pub(in crate::agent) fn latest_agent_message(&self, turn: u64) -> Option<&str> {
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
    pub(in crate::agent) fn turn_steps(&self, turn: u64) -> usize {
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

    pub(in crate::agent) fn is_working(&self) -> bool {
        self.working_started.is_some()
    }

    pub(in crate::agent) fn is_compacting(&self) -> bool {
        self.compacting
    }

    pub(in crate::agent) fn set_compacting(&mut self, compacting: bool, cx: &mut Context<Self>) {
        self.compacting = compacting;
        cx.notify();
    }

    pub(in crate::agent) fn was_interrupted(&self, turn: u64) -> bool {
        self.interrupted_turns.contains(&turn)
    }

    pub(in crate::agent) fn mark_interrupted(&mut self, turn: u64) {
        self.interrupted_turns.insert(turn);
    }

    pub(in crate::agent) fn start_working(&mut self, cx: &mut Context<Self>) {
        self.working_started = Some(Instant::now());
        self.working_output_tokens = None;
        // Whatever the last turn was doing has nothing to say about this one.
        self.working_detail = None;
        cx.notify();
    }

    /// Report what the running turn is doing, or clear it once the backend has
    /// nothing further to add.
    pub(in crate::agent) fn set_working_detail(
        &mut self,
        detail: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.working_detail != detail {
            self.working_detail = detail;
            cx.notify();
        }
    }

    pub(in crate::agent) fn set_working_output_tokens(
        &mut self,
        output_tokens: u64,
        cx: &mut Context<Self>,
    ) {
        if self.working_started.is_some() {
            self.working_output_tokens = Some(output_tokens);
            cx.notify();
        }
    }

    /// Settle the running turn's duration and output usage for its status row.
    /// These are view state rather than provider transcript content, so they
    /// stay outside the shared item stream.
    pub(in crate::agent) fn settle_turn(&mut self, turn: u64, cx: &mut Context<Self>) {
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
    pub(in crate::agent) fn discard_turn(&mut self, turn: u64, cx: &mut Context<Self>) {
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

impl Render for TranscriptView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = cx.global::<AppSettings>();
        let collapse = settings.collapse_tool_calls;
        self.transcript_list
            .set_smooth_wheel_enabled(settings.smooth_scrolling.agent_enabled());

        // Transcript rows, one folded/expanded section per turn (entries are
        // tagged with a monotonic turn id, so turns are contiguous slices).
        // Only the visible slice becomes elements; the spec diff tells the
        // list which rows changed shape.
        let specs = self.build_row_specs(collapse);
        self.sync_transcript_list(specs);

        let font = (
            cx.global::<AppSettings>().agent_font_family.clone(),
            cx.global::<AppSettings>().agent_font_size,
        );
        if self.transcript_font != font {
            self.transcript_font = font;
            self.transcript_list.remeasure();
        }

        let has_hidden_content_below = self.transcript_has_hidden_content_below();
        let scrolled_from_top = self.transcript_has_hidden_content_above();

        // The scrollbar must sit OUTSIDE the scrolling element (a child would
        // scroll away with the content), so a relative wrapper hosts the
        // scroll area and the overlay bar.
        div()
            .relative()
            .size_full()
            .min_h_0()
            .child(
                div()
                    .id("agent-transcript")
                    .size_full()
                    .on_prepaint({
                        let view = cx.entity().downgrade();
                        move |bounds, _, cx| {
                            view.update(cx, |this, cx| {
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
                        .pt(px(16.)),
                    ),
            )
            .children(scrolled_from_top.then(|| {
                // This decorative overlay has no handlers, so text selection
                // and wheel input continue to reach the list.
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right(px(16.))
                    .h(px(24.))
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(cx.theme().sidebar, 0.),
                        linear_color_stop(cx.theme().sidebar.opacity(0.), 1.),
                    ))
            }))
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
