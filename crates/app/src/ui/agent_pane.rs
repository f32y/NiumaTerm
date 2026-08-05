//! Agent tab: renders an agent conversation (Codex or Claude Code) as chat
//! bubbles instead of a terminal grid. All process and protocol handling
//! lives in [`nmt_agent_utils::codex::app_server`] and
//! [`nmt_agent_utils::claude_code::stream_json`]; this module only maps their
//! shared typed events onto the transcript UI.

use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use chrono::Local;
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::prelude::*;
use gpui::{
    AnyElement, ClipboardItem, Context, Div, Entity, FocusHandle, FontWeight, ListSizingBehavior,
    ScrollHandle, Window, div, px, relative, size,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::scroll::Scrollbar;
use gpui_component::skeleton::Skeleton;
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, VirtualListScrollHandle, h_flex, text, v_flex,
    v_virtual_list,
};
use nmt_agent_utils::chat::{
    Event as SessionEvent, Item as SessionItem, ModelInfo, ReplayItem, SendOutcome, SessionSummary,
    ThreadSettings,
};
use nmt_agent_utils::claude_code::{sessions, stream_json};
use nmt_agent_utils::codex::app_server;
use serde_json::Value;
use tracing::warn;

use crate::ui::AppSettings;

/// A transcript entry plus the local wall-clock time it first appeared
/// (shown on hover) and the turn it belongs to (drives turn folding).
/// Streamed items keep their start time.
struct Entry {
    at: String,
    turn: u64,
    item: ChatItem,
}

/// One entry in the conversation transcript. Streaming notifications mutate
/// the entry in place (keyed by the protocol's item id) until `item/completed`
/// finalizes it with the authoritative payload.
enum ChatItem {
    User {
        text: String,
    },
    Agent {
        item_id: String,
        text: String,
    },
    Reasoning {
        item_id: String,
        text: String,
    },
    Command {
        item_id: String,
        command: String,
        output: String,
        status: String,
        exit_code: Option<i64>,
    },
    FileChange {
        item_id: String,
        summary: String,
        status: String,
    },
    /// Fallback card for every other tool-call item kind (mcpToolCall,
    /// webSearch, dynamicToolCall, …): kind + best-effort title + status.
    Tool {
        item_id: String,
        kind: String,
        title: String,
        status: String,
    },
    /// Per-turn duration record; marks the turn as settled and renders as its
    /// "Worked for Ns" fold header. The live ticking line is a render-only
    /// row pinned to the transcript end (see `working_started`).
    Working {
        started: Instant,
        done_seconds: Option<u64>,
    },
    Error {
        text: String,
    },
}

#[derive(Clone, Copy, PartialEq)]
enum Status {
    Starting,
    Idle,
    Running,
    Exited,
}

/// Which agent backs this pane; the persisted tab snapshot stores the wire
/// name so future kinds can slot in without a schema change.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentKind {
    Codex,
    Claude,
}

impl AgentKind {
    pub(crate) fn wire(self) -> &'static str {
        match self {
            AgentKind::Codex => "codex",
            AgentKind::Claude => "claude",
        }
    }

    pub(crate) fn display(self) -> &'static str {
        match self {
            AgentKind::Codex => "Codex",
            AgentKind::Claude => "Claude",
        }
    }

    /// `None` for unknown kinds (a newer snapshot), which degrade to a plain
    /// terminal tab instead of losing the tab.
    pub(crate) fn from_wire(wire: &str) -> Option<Self> {
        match wire {
            "codex" => Some(AgentKind::Codex),
            "claude" => Some(AgentKind::Claude),
            _ => None,
        }
    }
}

/// The pane's protocol session, one variant per agent kind. Both backends
/// share the [`nmt_agent_utils::chat`] event vocabulary and method surface,
/// so the pane dispatches here and stays protocol-agnostic.
enum Backend {
    Codex(app_server::Session),
    Claude(stream_json::Session),
}

impl Backend {
    fn process(&mut self, message: Value) -> Vec<SessionEvent> {
        match self {
            Backend::Codex(session) => session.process(message),
            Backend::Claude(session) => session.process(message),
        }
    }

    fn send_user_message(&mut self, text: &str, settings: &ThreadSettings) -> SendOutcome {
        match self {
            Backend::Codex(session) => session.send_user_message(text, settings),
            Backend::Claude(session) => session.send_user_message(text, settings),
        }
    }

    fn interrupt(&mut self) {
        match self {
            Backend::Codex(session) => session.interrupt(),
            Backend::Claude(session) => session.interrupt(),
        }
    }

    fn respond_approval(&mut self, decision: &str) {
        match self {
            Backend::Codex(session) => session.respond_approval(decision),
            Backend::Claude(session) => session.respond_approval(decision),
        }
    }
}

pub(crate) struct AgentPane {
    pub(crate) focus: FocusHandle,
    kind: AgentKind,
    /// The tab's working directory; the session process runs here and the
    /// session history is scoped to it (resume ids only resolve against the
    /// same directory).
    cwd: Option<String>,
    items: Vec<Entry>,
    scroll: ScrollHandle,
    input: Entity<InputState>,
    session: Option<Backend>,
    /// Bumped on every (re)spawn; the message pump and EOF handler of an
    /// older session compare against it and stand down, so deliberately
    /// replacing the session (resume) doesn't route stale messages into the
    /// new one or report a bogus exit.
    session_epoch: u64,
    status: Status,
    /// Resumable sessions for this cwd, newest first; shown above the
    /// composer while the transcript is empty.
    history: Vec<SessionSummary>,
    /// Set between the cheap count pass and the title-parsing pass: the list
    /// reserves its final height with this many placeholder rows, so the
    /// composer doesn't jump when real rows land.
    history_pending: Option<usize>,
    /// The list is one-shot per tab: picking a session or sending the first
    /// message hides it for the rest of the tab's life.
    history_dismissed: bool,
    history_scroll: VirtualListScrollHandle,
    /// Description of the approval request blocking the turn, shown as the
    /// card above the input; the request id lives in the session.
    pending_approval: Option<String>,
    /// Current thread settings, seeded from the session's `Ready` event and
    /// changed via the dropdowns under the input; sent as overrides on every
    /// turn start (idempotent when unchanged).
    settings: ThreadSettings,
    /// Model catalog; service tiers are per model, so the tier dropdown lists
    /// the selected model's tiers.
    models: Vec<ModelInfo>,
    /// Collapsed work-log runs the user has expanded, keyed by the index of
    /// the run's first transcript entry (stable — the list only appends).
    expanded_groups: HashSet<usize>,
    /// Completed turns the user has unfolded (completed turns fold their
    /// intermediate work rows behind a "Worked for Ns" header by default).
    expanded_turns: HashSet<u64>,
    /// Work-log rows whose detail (command output, reasoning text) is
    /// expanded, keyed by transcript index.
    expanded_rows: HashSet<usize>,
    /// Monotonic turn counter; entries are tagged with the turn they arrived
    /// in so a settled turn can fold as one unit.
    turn_seq: u64,
    /// Start time of the running turn. While set, a ticking
    /// "Working for Ns" row renders at the transcript end; cleared into a
    /// permanent "Worked for Ns" fold header when the turn completes.
    working_started: Option<Instant>,
}

impl AgentPane {
    pub(crate) fn new(
        kind: AgentKind,
        cwd: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = kind.display();
        // Auto-grow wraps long prompts instead of scrolling them off-screen;
        // Enter still submits (submit_on_enter), Shift+Enter inserts a
        // newline.
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder(format!("Message {name} — Enter to send"))
        });

        cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            // Shift+Enter emits PressEnter too, but it inserted a newline —
            // only a plain Enter sends.
            if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                this.send_user_message(window, cx);
            }
        })
        .detach();

        let mut this = Self {
            focus: cx.focus_handle(),
            kind,
            cwd,
            items: Vec::new(),
            scroll: ScrollHandle::new(),
            input,
            session: None,
            session_epoch: 0,
            status: Status::Starting,
            history: Vec::new(),
            history_pending: None,
            history_dismissed: false,
            history_scroll: VirtualListScrollHandle::new(),
            pending_approval: None,
            settings: ThreadSettings::default(),
            models: Vec::new(),
            expanded_groups: HashSet::new(),
            expanded_turns: HashSet::new(),
            expanded_rows: HashSet::new(),
            turn_seq: 0,
            working_started: None,
        };

        this.start_session(None, cx);

        // Claude's history comes from scanning the CLI's transcript
        // directory (Codex delivers its history over the protocol instead,
        // via `Event::History`). Two passes, both off-thread: a cheap count
        // first, so the list can reserve its final height with placeholder
        // rows, then title parsing, which swaps in the real rows.
        if kind == AgentKind::Claude {
            let cwd = this.cwd.clone();

            cx.spawn(async move |this, cx| {
                let count_cwd = cwd.clone();
                let count = cx
                    .background_executor()
                    .spawn(async move { sessions::count_sessions(count_cwd.as_deref()) })
                    .await;

                let proceed = this
                    .update(cx, |this, cx| {
                        this.history_pending = Some(count);
                        cx.notify();

                        count > 0
                    })
                    .unwrap_or(false);

                if !proceed {
                    return;
                }

                // Title parsing races a short hold: on a warm SSD it
                // finishes within a frame, so without the hold the skeleton
                // rows would never be visible and the swap would read as a
                // flicker.
                let load = cx
                    .background_executor()
                    .spawn(async move { sessions::list_sessions(cwd.as_deref()) });

                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;

                let sessions = load.await;

                let _ = this.update(cx, |this, cx| {
                    this.history = sessions;
                    this.history_pending = None;
                    cx.notify();
                });
            })
            .detach();
        }

        this
    }

    /// Spawn the backend process (optionally resuming a persisted Claude
    /// session) and pump its messages onto the UI thread. Channel closure is
    /// the EOF signal (the sender is owned by the reader thread). Does not
    /// notify — callers decide whether a repaint is due.
    fn start_session(&mut self, resume: Option<String>, cx: &mut Context<Self>) {
        let kind = self.kind;
        let name = kind.display();
        let cwd = self.cwd.clone();

        self.session_epoch += 1;
        let epoch = self.session_epoch;

        let (tx, mut rx) = mpsc::unbounded::<Value>();
        let deliver = move |message| {
            let _ = tx.unbounded_send(message);
        };
        let spawned = match kind {
            AgentKind::Codex => {
                app_server::Session::spawn(cwd, deliver, |line| warn!("codex app-server: {line}"))
                    .map(Backend::Codex)
            }
            AgentKind::Claude => {
                stream_json::Session::spawn(cwd, resume, deliver, |line| warn!("claude: {line}"))
                    .map(Backend::Claude)
            }
        };

        match spawned {
            Ok(session) => {
                self.session = Some(session);
                self.status = Status::Starting;

                cx.spawn(async move |this, cx| {
                    while let Some(message) = rx.next().await {
                        let updated = this.update(cx, |this, cx| {
                            // A newer session owns the pane now; this pump's
                            // messages belong to the replaced process.
                            if this.session_epoch != epoch {
                                return false;
                            }

                            let events = match this.session.as_mut() {
                                Some(session) => session.process(message),
                                None => Vec::new(),
                            };

                            for event in events {
                                this.apply_event(event, cx);
                            }

                            true
                        });

                        if !updated.unwrap_or(false) {
                            return;
                        }
                    }

                    let _ = this.update(cx, |this, cx| {
                        // A deliberately replaced session exits by design;
                        // only the live session's death is worth a line.
                        if this.session_epoch != epoch {
                            return;
                        }
                        this.status = Status::Exited;
                        this.finish_working(cx);
                        this.push(
                            ChatItem::Error {
                                text: format!("{name} exited."),
                            },
                            cx,
                        );
                    });
                })
                .detach();
            }
            Err(err) => {
                self.status = Status::Exited;
                self.items.push(Entry {
                    at: Local::now().format("%H:%M").to_string(),
                    turn: self.turn_seq,
                    item: ChatItem::Error {
                        text: format!("Failed to start {name}: {err}"),
                    },
                });
            }
        }
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    pub(crate) fn kind(&self) -> AgentKind {
        self.kind
    }

    fn push(&mut self, item: ChatItem, cx: &mut Context<Self>) {
        self.items.push(Entry {
            at: Local::now().format("%H:%M").to_string(),
            turn: self.turn_seq,
            item,
        });
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn send_user_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().to_string();
        let text = text.trim().to_string();

        if text.is_empty() {
            return;
        }

        if self.send_text(text, cx) {
            self.input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    /// Send one user message through the session with full turn bookkeeping;
    /// also used for UI-generated messages such as the `/effort` command.
    /// Returns false when the session isn't ready yet.
    fn send_text(&mut self, text: String, cx: &mut Context<Self>) -> bool {
        let settings = self.settings.clone();
        let outcome = match self.session.as_mut() {
            Some(session) => session.send_user_message(&text, &settings),
            None => SendOutcome::NotReady,
        };

        if outcome == SendOutcome::NotReady {
            self.push(
                ChatItem::Error {
                    text: format!(
                        "{} is still starting; try again in a moment.",
                        self.kind.display()
                    ),
                },
                cx,
            );
            return false;
        }

        // The first message commits this tab to its conversation; the
        // history list is no longer offered.
        self.history_dismissed = true;

        // A steer joins the running turn (and its progress line); a fresh
        // turn advances the counter first so its entries fold as one unit.
        if outcome == SendOutcome::StartedTurn {
            self.turn_seq += 1;
        }

        self.push(ChatItem::User { text }, cx);

        if outcome == SendOutcome::StartedTurn {
            self.start_working(cx);
        }

        true
    }

    /// Start the turn clock and drive the once-a-second repaint of the live
    /// progress row; the ticker stops itself once `finish_working` clears it.
    fn start_working(&mut self, cx: &mut Context<Self>) {
        self.working_started = Some(Instant::now());
        self.scroll.scroll_to_bottom();
        cx.notify();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;

                let ticking = this.update(cx, |this, cx| {
                    if this.working_started.is_some() {
                        cx.notify();
                        true
                    } else {
                        false
                    }
                });

                if !ticking.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Record the finished turn's duration as a permanent transcript entry —
    /// appended last, so it sits below the turn's output cards.
    fn finish_working(&mut self, cx: &mut Context<Self>) {
        if let Some(started) = self.working_started.take() {
            self.push(
                ChatItem::Working {
                    started,
                    done_seconds: Some(started.elapsed().as_secs()),
                },
                cx,
            );
        }
    }

    fn interrupt(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.session.as_mut() {
            session.interrupt();
            cx.notify();
        }
    }

    fn respond_approval(&mut self, decision: &str, cx: &mut Context<Self>) {
        // The card is dismissed immediately for a snappy UI; the session's
        // `ApprovalResolved` confirmation is then a no-op.
        self.pending_approval = None;

        if let Some(session) = self.session.as_mut() {
            session.respond_approval(decision);
        }
        cx.notify();
    }

    /// Apply one typed session event to the transcript and status line.
    fn apply_event(&mut self, event: SessionEvent, cx: &mut Context<Self>) {
        match event {
            SessionEvent::Ready(settings) => {
                // Seed the settings dropdowns with the thread's effective
                // configuration so they show real values before any change.
                self.settings = settings;
                self.status = Status::Idle;
                cx.notify();
            }
            SessionEvent::Models(models) => {
                self.models = models;
                cx.notify();
            }
            SessionEvent::TurnStarted => {
                self.status = Status::Running;
                cx.notify();
            }
            SessionEvent::TurnCompleted { error } => {
                self.finish_working(cx);
                if self.status == Status::Running {
                    self.status = Status::Idle;
                }
                if let Some(text) = error {
                    self.push(ChatItem::Error { text }, cx);
                }
                cx.notify();
            }
            SessionEvent::ItemStarted(item) => self.start_item(item, cx),
            SessionEvent::ItemCompleted(item) => self.complete_item(item, cx),
            SessionEvent::AgentMessageDelta { item_id, delta } => {
                self.append_delta(&item_id, &delta, |item| match item {
                    ChatItem::Agent { text, .. } => Some(text),
                    _ => None,
                });
                cx.notify();
            }
            SessionEvent::ReasoningSummaryDelta { item_id, delta } => {
                self.append_delta(&item_id, &delta, |item| match item {
                    ChatItem::Reasoning { text, .. } => Some(text),
                    _ => None,
                });
                cx.notify();
            }
            SessionEvent::CommandOutputDelta { item_id, delta } => {
                self.append_delta(&item_id, &delta, |item| match item {
                    ChatItem::Command { output, .. } => Some(output),
                    _ => None,
                });
                cx.notify();
            }
            SessionEvent::ApprovalRequested { description } => {
                self.pending_approval = Some(description);
                self.scroll.scroll_to_bottom();
                cx.notify();
            }
            SessionEvent::ApprovalResolved => {
                self.pending_approval = None;
                cx.notify();
            }
            SessionEvent::Error { message, fatal } => {
                if fatal {
                    self.status = Status::Exited;
                }
                self.push(ChatItem::Error { text: message }, cx);
            }
            SessionEvent::History(sessions) => {
                // Pages accumulate: the first page lands in an empty list,
                // later cursor pages extend it.
                self.history.extend(sessions);
                cx.notify();
            }
            SessionEvent::Replay(items) => self.apply_replay(items, cx),
            // No status line in the UI anymore; the live working row and the
            // Stop button carry the running state.
            SessionEvent::StatusDetail(_) => {}
        }
    }

    /// Pre-fill the transcript with a resumed session's reconstructed
    /// conversation. Replay entries share one turn and carry no fold header,
    /// so they render as a plain chronological stream above the new turns.
    fn apply_replay(&mut self, replay: Vec<ReplayItem>, cx: &mut Context<Self>) {
        for (i, item) in replay.into_iter().enumerate() {
            let item = match item {
                ReplayItem::User { text } => ChatItem::User { text },
                ReplayItem::Agent { text } => ChatItem::Agent {
                    item_id: format!("replay-{i}"),
                    text,
                },
                ReplayItem::Tools { count } => ChatItem::Tool {
                    item_id: format!("replay-{i}"),
                    kind: format!("{count} tool call{}", if count == 1 { "" } else { "s" }),
                    title: String::new(),
                    status: "completed".to_string(),
                },
            };

            // Replayed entries predate this pane; they get no wall-clock
            // hover stamp.
            self.items.push(Entry {
                at: String::new(),
                turn: self.turn_seq,
                item,
            });
        }

        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Resume the picked history entry. Codex switches its live session onto
    /// the stored thread (the response replays the transcript); Claude
    /// replaces the untouched fresh process with one reloading the session,
    /// and replays the transcript from the session file in parallel.
    fn resume_session(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(summary) = self.history.get(index) else {
            return;
        };
        let id = summary.id.clone();

        self.history_dismissed = true;

        match self.kind {
            AgentKind::Codex => {
                if let Some(Backend::Codex(session)) = self.session.as_mut() {
                    session.resume_thread(&id);
                }
            }
            AgentKind::Claude => {
                let cwd = self.cwd.clone();
                let replay_id = id.clone();

                cx.spawn(async move |this, cx| {
                    let replay = cx
                        .background_executor()
                        .spawn(async move { sessions::load_replay(cwd.as_deref(), &replay_id) })
                        .await;

                    let _ = this.update(cx, |this, cx| this.apply_replay(replay, cx));
                })
                .detach();

                self.start_session(Some(id), cx);
            }
        }

        cx.notify();
    }

    fn start_item(&mut self, item: SessionItem, cx: &mut Context<Self>) {
        let chat_item = match item {
            // Echoes of our own turn input, already rendered locally on send.
            SessionItem::UserMessage => return,
            SessionItem::AgentMessage { id, text } => ChatItem::Agent {
                item_id: id,
                text: text.unwrap_or_default(),
            },
            SessionItem::Reasoning { id, summary } => ChatItem::Reasoning {
                item_id: id,
                text: summary.unwrap_or_default(),
            },
            SessionItem::CommandExecution {
                id,
                command,
                aggregated_output,
                status,
                exit_code,
            } => ChatItem::Command {
                item_id: id,
                command,
                output: aggregated_output.unwrap_or_default(),
                status: status.unwrap_or_else(|| "inProgress".to_string()),
                exit_code,
            },
            SessionItem::FileChange { id, paths, status } => ChatItem::FileChange {
                item_id: id,
                summary: paths,
                status: status.unwrap_or_else(|| "inProgress".to_string()),
            },
            SessionItem::Other {
                id,
                kind,
                title,
                status,
            } => ChatItem::Tool {
                item_id: id,
                kind,
                title,
                status: status.unwrap_or_else(|| "inProgress".to_string()),
            },
        };

        self.push(chat_item, cx);
    }

    fn complete_item(&mut self, item: SessionItem, cx: &mut Context<Self>) {
        let Some(id) = session_item_id(&item).map(str::to_owned) else {
            return;
        };

        let known = self
            .items
            .iter()
            .any(|entry| chat_item_id(&entry.item) == Some(id.as_str()));

        // A completed item this pane never saw start (e.g. joined mid-turn)
        // still gets a transcript entry.
        if !known {
            self.start_item(item.clone(), cx);
        }

        for entry in &mut self.items {
            if chat_item_id(&entry.item) != Some(id.as_str()) {
                continue;
            }

            match (&mut entry.item, &item) {
                (ChatItem::Agent { text, .. }, SessionItem::AgentMessage { text: full, .. }) => {
                    if let Some(full) = full {
                        *text = full.clone();
                    }
                }
                (
                    ChatItem::Command {
                        output,
                        status,
                        exit_code,
                        ..
                    },
                    SessionItem::CommandExecution {
                        aggregated_output,
                        status: new_status,
                        exit_code: new_exit,
                        ..
                    },
                ) => {
                    if let Some(full) = aggregated_output {
                        *output = full.clone();
                    }
                    if let Some(state) = new_status {
                        *status = state.clone();
                    }
                    *exit_code = *new_exit;
                }
                (
                    ChatItem::FileChange { status, .. },
                    SessionItem::FileChange {
                        status: new_status, ..
                    },
                )
                | (
                    ChatItem::Tool { status, .. },
                    SessionItem::Other {
                        status: new_status, ..
                    },
                ) => {
                    if let Some(state) = new_status {
                        *status = state.clone();
                    }
                }
                (ChatItem::Reasoning { text, .. }, SessionItem::Reasoning { summary, .. }) => {
                    // Some reasoning items never stream summary deltas; the
                    // completed payload's summary is the fallback. Items that
                    // stay empty are hidden by the renderer.
                    if text.is_empty()
                        && let Some(summary) = summary
                    {
                        *text = summary.clone();
                    }
                }
                _ => {}
            }
            break;
        }

        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn append_delta(
        &mut self,
        item_id: &str,
        delta: &str,
        select: fn(&mut ChatItem) -> Option<&mut String>,
    ) {
        for entry in &mut self.items {
            if chat_item_id(&entry.item) == Some(item_id)
                && let Some(text) = select(&mut entry.item)
            {
                text.push_str(delta);
                self.scroll.scroll_to_bottom();
                return;
            }
        }
    }
}

/// Item ids key streaming deltas to their transcript entry.
fn chat_item_id(item: &ChatItem) -> Option<&str> {
    match item {
        ChatItem::Agent { item_id, .. }
        | ChatItem::Reasoning { item_id, .. }
        | ChatItem::Command { item_id, .. }
        | ChatItem::FileChange { item_id, .. }
        | ChatItem::Tool { item_id, .. } => Some(item_id),
        ChatItem::User { .. } | ChatItem::Error { .. } | ChatItem::Working { .. } => None,
    }
}

/// The turn-duration line, live ("Working for 12s") and settled
/// ("Worked for 12s") forms.
fn working_label(started: Instant, done_seconds: Option<u64>) -> String {
    match done_seconds {
        Some(seconds) => format!("Worked for {seconds}s"),
        None => format!("Working for {}s", started.elapsed().as_secs()),
    }
}

/// Work-log rows: the single-line tool/thinking entries that participate in
/// "+N previous tool calls" run-collapsing. Conversation text never does.
fn is_work_row(item: &ChatItem) -> bool {
    matches!(
        item,
        ChatItem::Command { .. }
            | ChatItem::FileChange { .. }
            | ChatItem::Tool { .. }
            | ChatItem::Reasoning { .. }
    )
}

/// Entries with nothing to show (yet): an agent bubble before its first delta,
/// or a reasoning item that never streamed a summary. They render no row and
/// are transparent to work-run grouping, so an invisible entry can't split a
/// run of tool calls into two summary lines.
fn hidden(item: &ChatItem) -> bool {
    match item {
        ChatItem::Agent { text, .. } | ChatItem::Reasoning { text, .. } => text.trim().is_empty(),
        _ => false,
    }
}

/// First non-empty line, truncated, for a work row's one-line preview.
fn preview_line(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();

    if line.chars().count() > 84 {
        let mut preview: String = line.chars().take(84).collect();
        preview.push('…');
        preview
    } else {
        line.to_string()
    }
}

/// Full text of an entry for the right-click Copy action — the whole message,
/// independent of any partial selection or truncated preview.
fn entry_copy_text(item: &ChatItem) -> String {
    match item {
        ChatItem::User { text }
        | ChatItem::Error { text }
        | ChatItem::Agent { text, .. }
        | ChatItem::Reasoning { text, .. } => text.clone(),
        ChatItem::Command {
            command, output, ..
        } => format!("$ {command}\n{output}"),
        ChatItem::FileChange {
            summary, status, ..
        } => format!("Edit {summary} — {status}"),
        ChatItem::Tool {
            kind,
            title,
            status,
            ..
        } => format!("{kind} {title} — {status}"),
        ChatItem::Working {
            started,
            done_seconds,
        } => working_label(*started, *done_seconds),
    }
}

/// The protocol item id, used to match a completed item to its entry.
fn session_item_id(item: &SessionItem) -> Option<&str> {
    match item {
        SessionItem::UserMessage => None,
        SessionItem::AgentMessage { id, .. }
        | SessionItem::Reasoning { id, .. }
        | SessionItem::CommandExecution { id, .. }
        | SessionItem::FileChange { id, .. }
        | SessionItem::Other { id, .. } => Some(id),
    }
}

/// Compact "how long ago" label for a history row ("now", "5m", "3h", "2d").
fn relative_time(at: SystemTime) -> String {
    let seconds = at.elapsed().map(|d| d.as_secs()).unwrap_or(0);

    match seconds {
        0..60 => "now".to_string(),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86400),
    }
}

/// Icon mirroring the current permission/approval choice (t3code's runtime
/// mode iconography): closed lock = prompts on, pen = edits auto-approved,
/// pencil-ruler = plan mode, open lock = no prompts. Covers both Claude's
/// permission modes and Codex's approval policies.
fn permission_icon(value: Option<&str>) -> IconName {
    match value {
        Some("acceptEdits") => IconName::PenLine,
        Some("plan") => IconName::PencilRuler,
        Some("bypassPermissions") | Some("never") => IconName::LockOpen,
        _ => IconName::Lock,
    }
}

impl gpui::Focusable for AgentPane {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AgentPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let collapse = cx.global::<AppSettings>().collapse_tool_calls;

        // Transcript rows, one folded/expanded section per turn (entries are
        // tagged with a monotonic turn id, so turns are contiguous slices).
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut start = 0;

        while start < self.items.len() {
            let turn = self.items[start].turn;
            let mut end = start + 1;

            while end < self.items.len() && self.items[end].turn == turn {
                end += 1;
            }

            self.render_turn(turn, start, end, collapse, &mut rows, cx);
            start = end;
        }

        // Live progress row, pinned below everything the running turn has
        // produced; replaced by the turn's fold header on completion.
        if let Some(started) = self.working_started {
            rows.push(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .px_1()
                    .child(h_flex().gap_1().children((0..3).map(|i| {
                        div()
                            .size(px(4.))
                            .rounded_full()
                            .bg(cx.theme().muted_foreground.opacity(0.85 - 0.28 * i as f32))
                    })))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground.opacity(0.7))
                            .child(working_label(started, None)),
                    )
                    .into_any_element(),
            );
        }

        // A pending approval transforms the composer: the panel slots into
        // the shell's top (the shell's rounded frame clips it), and the
        // decision buttons escalate left to right.
        let approval = self.pending_approval.as_ref().map(|approval| {
            v_flex()
                .w_full()
                .px_4()
                .py_3()
                .gap_2()
                .border_b_1()
                .border_color(cx.theme().border.opacity(0.65))
                .bg(cx.theme().muted.opacity(0.2))
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().muted_foreground)
                        .child("PENDING APPROVAL"),
                )
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().background.opacity(0.7))
                        .text_sm()
                        .child(approval.clone()),
                )
                .child(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("approval-cancel")
                                .ghost()
                                .label("Cancel turn")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.respond_approval("cancel", cx)
                                })),
                        )
                        .child(
                            Button::new("approval-decline")
                                .outline()
                                .label("Decline")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.respond_approval("decline", cx)
                                })),
                        )
                        .child(
                            Button::new("approval-session")
                                .outline()
                                .label("Always allow this session")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.respond_approval("acceptForSession", cx)
                                })),
                        )
                        .child(
                            Button::new("approval-accept")
                                .primary()
                                .label("Approve once")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.respond_approval("accept", cx)
                                })),
                        ),
                )
        });

        let running = self.status == Status::Running;

        // The history list only makes sense while the tab is a blank slate:
        // no transcript yet and no conversation committed to. It shows as
        // placeholders once the count pass promises rows, then as real rows.
        let history_rows = self.history_pending.unwrap_or(self.history.len());
        let history = (self.items.is_empty() && history_rows > 0 && !self.history_dismissed)
            .then(|| self.render_history(cx));

        v_flex()
            .size_full()
            .track_focus(&self.focus)
            // The agent tab is a terminal surface stand-in, so it overrides the
            // chrome's UI font with its own configured font (Settings → Agent
            // Font), same as the terminal pane does with the terminal font.
            .font_family(cx.global::<AppSettings>().agent_font_family.clone())
            .text_size(px(cx.global::<AppSettings>().agent_font_size as f32))
            .child(
                div()
                    .id("agent-transcript")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(v_flex().w_full().p_3().gap_2().children(rows)),
            )
            .child({
                let layered = history.is_some();

                // Composer area: the history strip sits OUTSIDE the shell,
                // on the pane background; the shell (bordered, shadowed
                // card) then overlaps the strip's lower edge with a negative
                // margin. Front card over back strip is carried by the
                // border/shadow/surface contrast, exactly like t3code's
                // context strip (mirrored to the top edge).
                div().w_full().px_3().pb_3().pt_1().children(history).child(
                    v_flex()
                        .w_full()
                        .when(layered, |this| this.mt(px(-14.)))
                        .rounded(px(16.))
                        .overflow_hidden()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().popover)
                        .shadow_md()
                        .children(approval)
                        .child(
                            div()
                                .px_3()
                                .pt_3()
                                .pb_2()
                                // The prompt editor reads larger than the
                                // chrome around it (t3code uses 16px over
                                // a 14px UI); +2 keeps that ratio at any
                                // configured agent font size.
                                .text_size(px(
                                    cx.global::<AppSettings>().agent_font_size as f32 + 2.0
                                ))
                                .child(Input::new(&self.input).appearance(false)),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .px_2p5()
                                .pb_2p5()
                                .pt_0p5()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(div().flex_1().min_w_0().child(self.render_settings_row(cx)))
                                // Stop replaces Send in place while a
                                // turn runs.
                                .child(if running {
                                    Button::new("agent-send")
                                        .danger()
                                        .rounded(px(999.))
                                        .child(div().size(px(10.)).rounded_sm().bg(gpui::white()))
                                        .on_click(cx.listener(|this, _, _, cx| this.interrupt(cx)))
                                } else {
                                    Button::new("agent-send")
                                        .primary()
                                        .rounded(px(999.))
                                        .icon(IconName::ArrowUp)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.send_user_message(window, cx)
                                        }))
                                }),
                        ),
                )
            })
    }
}

impl AgentPane {
    /// Render one turn: user rows, then (for settled turns) a clickable
    /// "Worked for Ns" fold header hiding the intermediate work rows by
    /// default, then the final reply. Running turns render chronologically.
    fn render_turn(
        &self,
        turn: u64,
        start: usize,
        end: usize,
        collapse: bool,
        rows: &mut Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) {
        let fold_seconds = (start..end).find_map(|i| match &self.items[i].item {
            ChatItem::Working {
                done_seconds: Some(seconds),
                ..
            } => Some(*seconds),
            _ => None,
        });

        let Some(seconds) = fold_seconds else {
            // Running (or pre-thread) turn: plain chronological stream.
            self.render_stream(start, end, &|_| false, collapse, rows, cx);
            return;
        };

        let folded = !self.expanded_turns.contains(&turn);

        // The final reply stays visible when the turn folds; everything
        // between the prompt and the answer is what the fold hides.
        let final_agent = (start..end).rev().find(|&i| {
            matches!(&self.items[i].item, ChatItem::Agent { .. }) && !hidden(&self.items[i].item)
        });

        for i in start..end {
            if matches!(&self.items[i].item, ChatItem::User { .. }) {
                rows.push(self.render_entry_row(i, cx));
            }
        }

        rows.push(self.render_fold_header(turn, seconds, folded, cx));

        if folded {
            // Errors stay visible even inside a folded turn.
            for i in start..end {
                if matches!(&self.items[i].item, ChatItem::Error { .. }) {
                    rows.push(self.render_entry_row(i, cx));
                }
            }
        } else {
            let skip = |i: usize| {
                Some(i) == final_agent || matches!(&self.items[i].item, ChatItem::User { .. })
            };
            self.render_stream(start, end, &skip, collapse, rows, cx);
        }

        if let Some(i) = final_agent {
            rows.push(self.render_entry_row(i, cx));
        }
    }

    /// Chronological rows for a slice of the transcript, collapsing runs of
    /// consecutive work-log rows to the newest one plus a "+N previous tool
    /// calls" toggle (when the collapse setting is on). Hidden entries are
    /// transparent: they neither render nor split a run.
    fn render_stream(
        &self,
        start: usize,
        end: usize,
        skip: &dyn Fn(usize) -> bool,
        collapse: bool,
        rows: &mut Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) {
        let mut i = start;

        while i < end {
            let item = &self.items[i].item;

            if skip(i) || hidden(item) || matches!(item, ChatItem::Working { .. }) {
                i += 1;
                continue;
            }

            if !is_work_row(item) {
                rows.push(self.render_entry_row(i, cx));
                i += 1;
                continue;
            }

            // Extend the run across consecutive (possibly hidden) work rows.
            let run_start = i;
            let mut visible: Vec<usize> = Vec::new();
            let mut j = i;

            while j < end
                && !skip(j)
                && (hidden(&self.items[j].item) || is_work_row(&self.items[j].item))
            {
                if !hidden(&self.items[j].item) {
                    visible.push(j);
                }
                j += 1;
            }

            if collapse && visible.len() > 1 {
                let expanded = self.expanded_groups.contains(&run_start);

                rows.push(self.render_run_toggle(run_start, visible.len() - 1, expanded, cx));

                if expanded {
                    for &k in &visible {
                        rows.push(self.render_work_row(k, cx));
                    }
                } else if let Some(&last) = visible.last() {
                    rows.push(self.render_work_row(last, cx));
                }
            } else {
                for &k in &visible {
                    rows.push(self.render_work_row(k, cx));
                }
            }

            i = j;
        }
    }

    fn render_entry_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let entry = &self.items[index];

        match &entry.item {
            ChatItem::User { text } => self.render_user_row(index, text.clone(), cx),
            ChatItem::Agent { text, .. } => self.render_agent_row(index, text.clone(), cx),
            ChatItem::Error { text } => self.render_error_row(index, text.clone(), cx),
            item if is_work_row(item) => self.render_work_row(index, cx),
            // Working entries render as the turn's fold header, never here.
            _ => div().into_any_element(),
        }
    }

    /// Hover-revealed timestamp; the row declares `.group("entry")`.
    fn hover_stamp(&self, index: usize, cx: &mut Context<Self>) -> Div {
        div()
            .flex_none()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .invisible()
            .group_hover("entry", |this| this.visible())
            .child(self.items[index].at.clone())
    }

    fn copy_menu(
        index: usize,
        item: &ChatItem,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let _ = index;
        let copy_text = entry_copy_text(item);

        move |menu, _, _| {
            let copy_text = copy_text.clone();
            menu.item(PopupMenuItem::new("Copy").on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
            }))
        }
    }

    /// User prompt: right-aligned quiet bubble (muted surface, no border).
    fn render_user_row(&self, index: usize, text: String, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .id(("entry", index))
            .group("entry")
            .w_full()
            .justify_end()
            .items_end()
            .gap_2()
            .context_menu(Self::copy_menu(index, &self.items[index].item))
            .child(self.hover_stamp(index, cx))
            .child(
                div()
                    .max_w(relative(0.8))
                    .px_3()
                    .py_2()
                    .rounded(cx.theme().radius_lg)
                    .bg(cx.theme().muted)
                    .child(text),
            )
            .into_any_element()
    }

    /// Assistant reply: full-width bare markdown — no bubble, no border;
    /// alignment and surface carry the distinction.
    fn render_agent_row(&self, index: usize, text: String, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .id(("entry", index))
            .group("entry")
            .w_full()
            .items_end()
            .gap_2()
            .context_menu(Self::copy_menu(index, &self.items[index].item))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px_1()
                    .child(text::TextView::markdown(("agent-md", index), text).selectable(true)),
            )
            .child(self.hover_stamp(index, cx))
            .into_any_element()
    }

    fn render_error_row(&self, index: usize, text: String, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .id(("entry", index))
            .w_full()
            .context_menu(Self::copy_menu(index, &self.items[index].item))
            .child(
                div()
                    .max_w(relative(0.9))
                    .px_3()
                    .py_2()
                    .rounded(cx.theme().radius_lg)
                    .bg(cx.theme().danger.opacity(0.15))
                    .text_color(cx.theme().danger)
                    .text_sm()
                    .child(text),
            )
            .into_any_element()
    }

    /// One work-log line: icon · heading · dimmed preview · chevron · status.
    /// Rows with detail (command output, reasoning text) expand on click into
    /// an indented block behind a left hairline rail.
    fn render_work_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let (icon, heading, preview, status, detail) = match &self.items[index].item {
            ChatItem::Command {
                command,
                output,
                status,
                exit_code,
                ..
            } => {
                // Belt and braces: a non-zero exit code is a failure even if
                // the provider reported the execution as completed.
                let failed = matches!(status.as_str(), "failed" | "declined")
                    || exit_code.is_some_and(|code| code != 0);
                let state = if failed { "failed" } else { status.as_str() };

                (
                    IconName::SquareTerminal,
                    command.clone(),
                    preview_line(output),
                    Some(state.to_string()),
                    (!output.trim().is_empty()).then(|| output.clone()),
                )
            }
            ChatItem::FileChange {
                summary, status, ..
            } => (
                IconName::File,
                "Edit".to_string(),
                summary.clone(),
                Some(status.clone()),
                None,
            ),
            ChatItem::Tool {
                kind,
                title,
                status,
                ..
            } => (
                if kind == "webSearch" {
                    IconName::Globe
                } else {
                    IconName::Settings2
                },
                kind.clone(),
                title.clone(),
                Some(status.clone()),
                None,
            ),
            ChatItem::Reasoning { text, .. } => (
                IconName::Bot,
                "Thinking".to_string(),
                preview_line(text),
                None,
                Some(text.clone()),
            ),
            _ => return div().into_any_element(),
        };

        let expandable = detail.is_some();
        let expanded = expandable && self.expanded_rows.contains(&index);
        let hover_bg = cx.theme().muted.opacity(0.4);

        let status_glyph = status.map(|state| {
            let (name, color) = match state.as_str() {
                "failed" | "declined" => (IconName::CircleX, cx.theme().danger),
                "completed" => (IconName::Check, cx.theme().muted_foreground),
                _ => (IconName::Minus, cx.theme().muted_foreground.opacity(0.6)),
            };

            Icon::new(name).size_3().text_color(color)
        });

        let header = h_flex()
            .id(("wl-head", index))
            .w_full()
            .gap_2()
            .items_center()
            .px_1()
            .py_0p5()
            .rounded(cx.theme().radius)
            .when(expandable, |this| {
                this.cursor_pointer()
                    .hover(move |style| style.bg(hover_bg))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.expanded_rows.insert(index) {
                            this.expanded_rows.remove(&index);
                        }
                        cx.notify();
                    }))
            })
            .child(
                Icon::new(icon)
                    .size_3p5()
                    .text_color(cx.theme().muted_foreground.opacity(0.8)),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(relative(0.6))
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().foreground.opacity(0.82))
                    .child(heading),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground.opacity(0.55))
                    .child(preview),
            )
            .children(expandable.then(|| {
                Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size_3()
                .text_color(cx.theme().muted_foreground.opacity(0.7))
            }))
            .children(status_glyph);

        v_flex()
            .id(("entry", index))
            .w_full()
            .context_menu(Self::copy_menu(index, &self.items[index].item))
            .child(header)
            .children(detail.filter(|_| expanded).map(|detail| {
                div()
                    .ml(px(28.))
                    .mt_1()
                    .border_l_1()
                    .border_color(cx.theme().border.opacity(0.45))
                    .pl_3()
                    .child(
                        div()
                            .id(("wl-out", index))
                            .max_h(px(256.))
                            .overflow_y_scroll()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(detail),
                    )
            }))
            .into_any_element()
    }

    /// The settled turn's "Worked for Ns" disclosure line, doubling as a
    /// section divider (bottom hairline).
    fn render_fold_header(
        &self,
        turn: u64,
        seconds: u64,
        folded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let foreground = cx.theme().foreground;
        let chevron = if folded {
            IconName::ChevronRight
        } else {
            IconName::ChevronDown
        };

        v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .id(("turn-fold", turn as usize))
                    .gap_1()
                    .items_center()
                    .px_1()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .hover(move |style| style.text_color(foreground))
                    .child(format!("Worked for {seconds}s"))
                    .child(Icon::new(chevron).size_3p5())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.expanded_turns.insert(turn) {
                            this.expanded_turns.remove(&turn);
                        }
                        cx.notify();
                    })),
            )
            .child(div().w_full().h(px(1.)).bg(cx.theme().border.opacity(0.6)))
            .into_any_element()
    }

    /// The "+N previous tool calls" / "Show fewer tool calls" toggle above the
    /// newest row of a collapsed work run.
    fn render_run_toggle(
        &self,
        run_start: usize,
        hidden_count: usize,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover_bg = cx.theme().muted.opacity(0.4);
        let label = if expanded {
            "Show fewer tool calls".to_string()
        } else {
            format!(
                "+{hidden_count} previous tool call{}",
                if hidden_count == 1 { "" } else { "s" }
            )
        };

        h_flex()
            .id(("wl-run", run_start))
            .gap_1()
            .items_center()
            .px_1()
            .py_0p5()
            .rounded(cx.theme().radius)
            .cursor_pointer()
            .hover(move |style| style.bg(hover_bg))
            .text_sm()
            .text_color(cx.theme().foreground.opacity(0.82))
            .child(
                Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size_3()
                .text_color(cx.theme().muted_foreground.opacity(0.7)),
            )
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.expanded_groups.insert(run_start) {
                    this.expanded_groups.remove(&run_start);
                }
                cx.notify();
            }))
            .into_any_element()
    }

    /// Height of one history row; all rows are uniform, which is what lets
    /// the virtual list precompute its scroll geometry.
    const HISTORY_ROW_HEIGHT: f32 = 28.0;
    /// Ten rows visible by default; more scroll within the fixed viewport.
    const HISTORY_MAX_HEIGHT: f32 = Self::HISTORY_ROW_HEIGHT * 10.0;

    /// The resumable-sessions block slotted into the composer shell above the
    /// input: a strip at 90% of the composer width on a slightly deeper
    /// surface, reading as a layer tucked behind the input card (t3code's
    /// context-strip look). While only the count pass has finished it shows
    /// skeleton rows at the final height, so the composer doesn't jump when
    /// the real rows land; rows render through a virtual list, so hundreds
    /// of persisted sessions cost only the visible ten.
    fn render_history(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let rows = self.history_pending.unwrap_or(self.history.len());

        let body: AnyElement = if self.history_pending.is_some() {
            // Height reserved from the count; content still loading.
            v_flex()
                .w_full()
                .px_2()
                .gap_0()
                .children((0..rows.min(10)).map(|i| {
                    h_flex()
                        .h(px(Self::HISTORY_ROW_HEIGHT))
                        .w_full()
                        .px_2()
                        .items_center()
                        .child(
                            Skeleton::new()
                                .h(px(14.))
                                .w(relative(if i % 2 == 0 { 0.72 } else { 0.55 }))
                                .rounded(px(4.)),
                        )
                }))
                .into_any_element()
        } else {
            let row_sizes = Rc::new(vec![size(px(0.), px(Self::HISTORY_ROW_HEIGHT)); rows]);

            div()
                .relative()
                .w_full()
                .max_h(px(Self::HISTORY_MAX_HEIGHT))
                .overflow_hidden()
                .px_2()
                .child(
                    v_virtual_list(
                        cx.entity(),
                        "agent-history",
                        row_sizes,
                        move |this, visible_range, _, cx| {
                            // The final page in view is the cue to fetch
                            // the next one (no-op without a cursor, and
                            // only Codex pages over the wire).
                            if visible_range.end >= this.history.len()
                                && let Some(Backend::Codex(session)) = this.session.as_mut()
                            {
                                session.request_more_history();
                            }

                            visible_range
                                .map(|index| this.render_history_row(index, cx))
                                .collect()
                        },
                    )
                    .track_scroll(&self.history_scroll)
                    .with_sizing_behavior(ListSizingBehavior::Infer),
                )
                .child(Scrollbar::vertical(&self.history_scroll))
                .into_any_element()
        };

        // Centered at 90% of the composer width, on the pane background
        // behind the shell: an outlined strip on a deeper tint, rounded only
        // at the top. The shell overlaps its lower edge (negative margin on
        // the shell), so the strip reads as a layer sliding out from behind
        // the front card. The extra bottom padding is clearance for that
        // overlap — without it the card would cover the last row.
        div().w_full().flex().justify_center().child(
            v_flex()
                .w(relative(0.95))
                .rounded_t(px(12.))
                .border_1()
                .border_b_0()
                .border_color(cx.theme().border.opacity(0.6))
                .bg(cx.theme().muted.opacity(0.55))
                .pb(px(20.))
                .child(
                    div()
                        .px_4()
                        .pt_2()
                        .pb_1()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().muted_foreground)
                        .child("RECENT SESSIONS"),
                )
                .child(body),
        )
    }

    /// One history row: title, branch, and relative time, in the settings
    /// row's ghost-control idiom (small, muted, hover lifts the foreground).
    fn render_history_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(session) = self.history.get(index) else {
            return div().into_any_element();
        };
        let hover_bg = cx.theme().muted.opacity(0.4);

        h_flex()
            .id(("history-row", index))
            .h(px(Self::HISTORY_ROW_HEIGHT))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .rounded(cx.theme().radius)
            .cursor_pointer()
            .hover(move |style| style.bg(hover_bg))
            .on_click(cx.listener(move |this, _, _, cx| this.resume_session(index, cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().foreground.opacity(0.82))
                    .child(session.title.clone()),
            )
            .children(session.branch.clone().map(|branch| {
                h_flex()
                    .flex_none()
                    .gap_1()
                    .items_center()
                    .max_w(px(180.))
                    .child(
                        Icon::new(IconName::GitBranch)
                            .size_3()
                            .text_color(cx.theme().muted_foreground.opacity(0.7)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground.opacity(0.7))
                            .child(branch),
                    )
            }))
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground.opacity(0.55))
                    .child(relative_time(session.last_active)),
            )
            .into_any_element()
    }

    /// The dropdown row under the input, per agent kind.
    fn render_settings_row(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.kind {
            AgentKind::Codex => self.render_codex_settings_row(cx).into_any_element(),
            AgentKind::Claude => self.render_claude_settings_row(cx).into_any_element(),
        }
    }

    /// Claude settings: model, permission mode, and reasoning effort. The
    /// model catalog comes from the initialize handshake; model and
    /// permission changes apply via control requests before the next message.
    /// Effort has no control request — it's applied by sending the `/effort`
    /// slash command as a user message, which the CLI handles locally
    /// (instant, no model call), so the picker sends it immediately as its
    /// own mini-turn. The effort levels are per model; models without effort
    /// support (e.g. Haiku) get no picker.
    fn render_claude_settings_row(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let model_options: Vec<(String, String)> = self
            .models
            .iter()
            .map(|m| (m.model.clone(), m.display.clone()))
            .collect();
        let permission_options: Vec<(String, String)> = stream_json::PERMISSION_OPTIONS
            .iter()
            .map(|v| (v.to_string(), v.to_string()))
            .collect();
        let effort_options: Vec<(String, String)> = self
            .models
            .iter()
            .find(|m| Some(&m.model) == self.settings.model.as_ref())
            .map(|m| m.efforts.iter().map(|v| (v.clone(), v.clone())).collect())
            .unwrap_or_default();

        let mut row = h_flex()
            .w_full()
            .gap_1()
            .flex_wrap()
            .text_color(cx.theme().muted_foreground)
            .child(Self::setting_picker(
                cx,
                "agent-model",
                "model",
                IconName::Cpu,
                self.settings.model.clone(),
                model_options,
                |this, value, _| this.settings.model = Some(value),
            ))
            .child(Self::setting_picker(
                cx,
                "agent-permission",
                "permissions",
                permission_icon(self.settings.approval.as_deref()),
                self.settings.approval.clone(),
                permission_options,
                |this, value, _| this.settings.approval = Some(value),
            ));

        if !effort_options.is_empty() {
            row = row.child(Self::setting_picker(
                cx,
                "agent-effort",
                "effort",
                IconName::Gauge,
                self.settings.effort.clone(),
                effort_options,
                |this, value, cx| {
                    this.settings.effort = Some(value.clone());
                    this.send_text(format!("/effort {value}"), cx);
                },
            ));
        }

        row
    }

    /// Codex settings: model, approval policy, sandbox, reasoning effort, and
    /// service tier. Values are thread settings sent as overrides on the next
    /// `turn/start`.
    fn render_codex_settings_row(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let model_options: Vec<(String, String)> = self
            .models
            .iter()
            .map(|m| (m.model.clone(), m.display.clone()))
            .collect();
        // Service tiers are per model, and the catalog only lists the
        // additional tiers (e.g. "Fast") — the normal tier is implicit, so
        // the menu carries a synthetic entry for it. Empty wire value =
        // normal = explicit `serviceTier: null` on the next turn.
        let mut tier_options: Vec<(String, String)> = vec![(String::new(), "normal".to_string())];

        tier_options.extend(
            self.models
                .iter()
                .find(|m| Some(&m.model) == self.settings.model.as_ref())
                .map(|m| m.tiers.clone())
                .unwrap_or_default(),
        );
        let approval_options: Vec<(String, String)> = app_server::APPROVAL_OPTIONS
            .iter()
            .map(|v| (v.to_string(), v.to_string()))
            .collect();
        let sandbox_options: Vec<(String, String)> = app_server::SANDBOX_OPTIONS
            .iter()
            .map(|(v, label)| (v.to_string(), label.to_string()))
            .collect();
        let effort_options: Vec<(String, String)> = app_server::EFFORT_OPTIONS
            .iter()
            .map(|v| (v.to_string(), v.to_string()))
            .collect();

        h_flex()
            .w_full()
            .gap_1()
            .flex_wrap()
            .text_color(cx.theme().muted_foreground)
            .child(Self::setting_picker(
                cx,
                "agent-model",
                "model",
                IconName::Cpu,
                self.settings.model.clone(),
                model_options,
                |this, value, _| {
                    // A tier the new model doesn't offer falls back to that
                    // model's default tier instead of erroring the next turn.
                    if let Some(info) = this.models.iter().find(|m| m.model == value)
                        && !this
                            .settings
                            .tier
                            .as_ref()
                            .is_some_and(|tier| info.tiers.iter().any(|(id, _)| id == tier))
                    {
                        this.settings.tier = info.default_tier.clone();
                    }
                    this.settings.model = Some(value);
                },
            ))
            .child(Self::setting_picker(
                cx,
                "agent-approval",
                "approval",
                permission_icon(self.settings.approval.as_deref()),
                self.settings.approval.clone(),
                approval_options,
                |this, value, _| this.settings.approval = Some(value),
            ))
            .child(Self::setting_picker(
                cx,
                "agent-sandbox",
                "sandbox",
                IconName::Shield,
                self.settings.sandbox.clone(),
                sandbox_options,
                |this, value, _| this.settings.sandbox = Some(value),
            ))
            .child(Self::setting_picker(
                cx,
                "agent-effort",
                "effort",
                IconName::Gauge,
                self.settings.effort.clone(),
                effort_options,
                |this, value, _| this.settings.effort = Some(value),
            ))
            .child(Self::setting_picker(
                cx,
                "agent-tier",
                "tier",
                IconName::Zap,
                Some(self.settings.tier.clone().unwrap_or_default()),
                tier_options,
                |this, value, _| {
                    this.settings.tier = (!value.is_empty()).then_some(value);
                },
            ))
    }

    /// One dropdown: a ghost button showing `icon · current value · chevron`
    /// (t3code's composer-control look — the icon carries the control's
    /// meaning, the tooltip its name) whose menu lists `(wire value, display
    /// label)` options; picking one stores the wire value via `set`.
    fn setting_picker(
        cx: &mut Context<Self>,
        id: &'static str,
        name: &'static str,
        icon: IconName,
        current: Option<String>,
        options: Vec<(String, String)>,
        set: fn(&mut Self, String, &mut Context<Self>),
    ) -> impl IntoElement + use<> {
        let pane = cx.entity();

        // Show the display label of the current wire value when we know it.
        let current_label = current
            .as_ref()
            .map(|value| {
                options
                    .iter()
                    .find(|(wire, _)| wire == value)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_else(|| value.clone())
            })
            .unwrap_or_else(|| "—".to_string());

        Button::new(id)
            .ghost()
            .small()
            .tooltip(name)
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Icon::new(icon)
                            .size_4()
                            .text_color(cx.theme().muted_foreground.opacity(0.8)),
                    )
                    .child(div().text_sm().child(current_label))
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size_3()
                            .text_color(cx.theme().muted_foreground.opacity(0.7)),
                    ),
            )
            // Anchored bottom-left so the menu opens upward — the row sits at
            // the bottom edge of the pane.
            .dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, _, _| {
                let mut menu = menu;

                if options.is_empty() {
                    menu = menu.label("loading…");
                }

                for (value, label) in options.clone() {
                    let pane = pane.clone();
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        pane.update(cx, |this, cx| {
                            set(this, value.clone(), cx);
                            cx.notify();
                        });
                    }));
                }

                menu
            })
    }
}
