//! Agent tab: renders an agent conversation (Codex or Claude Code) as chat
//! bubbles instead of a terminal grid. All process and protocol handling
//! lives in [`nmt_agent_utils::codex::app_server`] and
//! [`nmt_agent_utils::claude_code::stream_json`]; this module only maps their
//! shared typed events onto the transcript UI.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::env;
use std::mem::take;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use chrono::Local;
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::prelude::*;
use gpui::{
    AnyElement, ClipboardItem, Context, Div, Entity, FocusHandle, FollowMode, FontWeight,
    ListAlignment, ListSizingBehavior, ListState, MouseButton, ScrollHandle, SharedString, Window,
    div, list, px, relative, size,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{
    Enter, Escape, IndentInline, Input, InputEvent, InputState, MoveDown, MoveUp,
};
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::scroll::Scrollbar;
use gpui_component::skeleton::Skeleton;
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, VirtualListScrollHandle, h_flex, text, v_flex,
    v_virtual_list,
};
use nmt_agent_utils::chat::{
    ContextWindowUsage, Event as SessionEvent, Item as SessionItem, ModelInfo, ReplayItem,
    SendOutcome, SessionSummary, SkillCatalog, SkillInfo, SkillReference, SlashCommandArguments,
    SlashCommandInfo, SlashCommandOutcome, SlashCommandRunPolicy, SlashCommandSource,
    ThreadSettings,
};
use nmt_agent_utils::claude_code::{sessions, stream_json};
use nmt_agent_utils::codex::app_server;
use nmt_agent_utils::{
    AgentEvent, AgentEventKind, AgentRoute, agent_process, normalize_body, normalize_title,
};
use nmt_config::local_state::AgentDefaults as StoredAgentDefaults;
use serde_json::Value;
use tracing::warn;

use crate::ui::AppSettings;
use crate::ui::agent_commands::{
    PaletteCatalogEntry, PaletteDirection, claim_command_turn_start, filter_palette_catalog,
    filter_skill_catalog, local_commands, merge_catalog, move_palette_selection,
    next_session_epoch, parse_slash_command, prepare_skill_selection, reconcile_skill_binding,
    reset_command_runtime, resolve_choice, validate_skill_binding,
};
use crate::ui::git_status::current_branch;

#[derive(Clone)]
struct PendingSlashCommand {
    name: String,
    arguments: String,
}

#[derive(Clone)]
enum CommandFeedbackKind {
    Notice,
    Error,
    Queued,
}

#[derive(Clone)]
struct CommandFeedback {
    kind: CommandFeedbackKind,
    message: String,
}

#[derive(Clone)]
enum PaletteAction {
    Command(SlashCommandInfo),
    Choice { command: String, value: String },
    Skill(SkillInfo),
}

#[derive(Clone)]
struct PaletteRow {
    label: String,
    description: String,
    hint: Option<String>,
    disabled_reason: Option<String>,
    action: PaletteAction,
}

struct PaletteModel {
    rows: Vec<PaletteRow>,
    note: Option<String>,
}

#[derive(Clone, Copy)]
enum PaletteControl {
    Previous,
    Next,
    Activate,
    Complete,
    Dismiss,
}

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
        /// Reviewable diff body when the provider exposes one; renders as
        /// the row's expandable detail with diff highlighting.
        diff: Option<String>,
        status: String,
    },
    /// Fallback card for every other tool-call item kind (mcpToolCall,
    /// webSearch, dynamicToolCall, …): kind + best-effort title + status.
    Tool {
        item_id: String,
        kind: String,
        title: String,
        /// Result payload (search matches, fetched content, …), delivered on
        /// completion; renders as the row's expandable detail.
        output: Option<String>,
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

#[derive(Clone)]
pub(crate) enum AgentPaneEvent {
    Lifecycle(AgentEvent),
    Interrupted,
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

/// Last-chosen thread settings per agent wire name, seeding the dropdowns of
/// newly opened (non-resumed) agent conversations. Loaded from
/// local_state.toml at startup and flushed back on quit.
#[derive(Default)]
pub(crate) struct AgentThreadDefaults(pub(crate) HashMap<String, ThreadSettings>);

impl gpui::Global for AgentThreadDefaults {}

impl AgentThreadDefaults {
    pub(crate) fn from_local_state(stored: &BTreeMap<String, StoredAgentDefaults>) -> Self {
        Self(
            stored
                .iter()
                .map(|(kind, d)| {
                    (
                        kind.clone(),
                        ThreadSettings {
                            model: d.model.clone(),
                            approval: d.approval.clone(),
                            sandbox: d.sandbox.clone(),
                            effort: d.effort.clone(),
                            tier: d.tier.clone(),
                        },
                    )
                })
                .collect(),
        )
    }

    pub(crate) fn to_local_state(&self) -> BTreeMap<String, StoredAgentDefaults> {
        self.0
            .iter()
            .map(|(kind, s)| {
                (
                    kind.clone(),
                    StoredAgentDefaults {
                        model: s.model.clone(),
                        approval: s.approval.clone(),
                        sandbox: s.sandbox.clone(),
                        effort: s.effort.clone(),
                        tier: s.tier.clone(),
                    },
                )
            })
            .collect()
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

    fn send_user_message(
        &mut self,
        text: &str,
        settings: &ThreadSettings,
        skill: Option<&SkillReference>,
    ) -> SendOutcome {
        match self {
            Backend::Codex(session) => session.send_user_message_with_skill(text, settings, skill),
            Backend::Claude(session) => session.send_user_message(text, settings),
        }
    }

    fn adapter_commands(&self) -> Vec<SlashCommandInfo> {
        match self {
            Backend::Codex(_) => app_server::Session::adapter_commands(),
            Backend::Claude(_) => stream_json::Session::adapter_commands(),
        }
    }

    fn execute_slash_command(&mut self, name: &str, arguments: &str) -> SlashCommandOutcome {
        match self {
            Backend::Codex(session) => session.execute_slash_command(name, arguments),
            Backend::Claude(session) => session.execute_slash_command(name, arguments),
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
    agent_route: AgentRoute,
    kind: AgentKind,
    /// The tab's working directory; the session process runs here and the
    /// session history is scoped to it (resume ids only resolve against the
    /// same directory).
    cwd: Option<String>,
    items: Vec<Entry>,
    /// Virtualized transcript: only visible rows build elements each frame.
    /// `row_specs` mirrors the list's item count; render() diffs freshly
    /// built specs against it and splices/remeasures just the changed range.
    transcript_list: ListState,
    row_specs: Vec<RowSpec>,
    /// Row heights depend on the agent font, which the specs can't see; the
    /// last-seen font triggers a full remeasure when it changes.
    transcript_font: (SharedString, f64),
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
    /// Whether the next `Ready` should overlay the remembered per-kind
    /// defaults onto the backend's reported configuration. True for fresh
    /// conversations; resumed threads keep their own stored settings.
    seed_thread_defaults: bool,
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
    /// Provider discovery is a replacement snapshot; adapter/local entries
    /// remain available independently of whether discovery has arrived.
    provider_commands: Vec<SlashCommandInfo>,
    provider_commands_ready: bool,
    /// `None` means Codex discovery is still loading. A populated catalog can
    /// contain both usable skills and non-fatal per-file errors.
    skill_catalog: Option<SkillCatalog>,
    /// Exact picker identity retained while the composer keeps its `$name`
    /// token. It is validated against `skill_catalog` before every send.
    skill_binding: Option<SkillReference>,
    palette_selected: usize,
    palette_dismissed: bool,
    palette_scroll: ScrollHandle,
    command_feedback: Option<CommandFeedback>,
    command_queue: VecDeque<PendingSlashCommand>,
    /// An accepted backend command starts the progress clock only after the
    /// protocol reports a real turn, not when the request is written.
    awaiting_command_turn: bool,
    git_branch: Option<String>,
    git_branch_ready: bool,
    git_branch_refreshing: bool,
    context_window_usage: Option<ContextWindowUsage>,
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
            } else if matches!(event, InputEvent::Change) {
                let text = this.input.read(cx).text().to_string();
                reconcile_skill_binding(&text, &mut this.skill_binding);
                this.palette_selected = 0;
                this.palette_dismissed = false;
                if !matches!(
                    this.command_feedback
                        .as_ref()
                        .map(|feedback| &feedback.kind),
                    Some(CommandFeedbackKind::Queued)
                ) {
                    this.command_feedback = None;
                }
                cx.notify();
            }
        })
        .detach();

        let mut this = Self {
            focus: cx.focus_handle(),
            agent_route: agent_process().allocate_route(),
            kind,
            cwd,
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
            seed_thread_defaults: true,
            models: Vec::new(),
            expanded_groups: HashSet::new(),
            expanded_turns: HashSet::new(),
            expanded_rows: HashSet::new(),
            turn_seq: 0,
            working_started: None,
            provider_commands: Vec::new(),
            provider_commands_ready: kind == AgentKind::Codex,
            skill_catalog: None,
            skill_binding: None,
            palette_selected: 0,
            palette_dismissed: false,
            palette_scroll: ScrollHandle::new(),
            command_feedback: None,
            command_queue: VecDeque::new(),
            awaiting_command_turn: false,
            git_branch: None,
            git_branch_ready: false,
            git_branch_refreshing: false,
            context_window_usage: None,
        };

        this.start_session(None, cx);
        this.refresh_git_branch(cx);

        cx.spawn(async move |this, cx| {
            loop {
                let Ok(interval) = this.update(cx, |_, cx| {
                    cx.global::<AppSettings>().git_status_refresh_interval
                }) else {
                    break;
                };

                cx.background_executor()
                    .timer(Duration::from_secs(interval.max(1)))
                    .await;

                if this
                    .update(cx, |this, cx| this.refresh_git_branch(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

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

    pub(crate) fn agent_route(&self) -> &AgentRoute {
        &self.agent_route
    }

    fn refresh_git_branch(&mut self, cx: &mut Context<Self>) {
        if self.git_branch_refreshing {
            return;
        }

        let Some(cwd) = self.cwd.clone().or_else(|| {
            env::current_dir()
                .ok()
                .map(|path| path.to_string_lossy().to_string())
        }) else {
            self.git_branch = None;
            self.git_branch_ready = true;
            return;
        };

        self.git_branch_refreshing = true;

        let fetch = cx
            .background_executor()
            .spawn(async move { current_branch(&cwd) });

        cx.spawn(async move |this, cx| {
            let branch = fetch.await;

            this.update(cx, |this, cx| {
                this.git_branch = branch;
                this.git_branch_ready = true;
                this.git_branch_refreshing = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn emit_lifecycle(
        &self,
        kind: AgentEventKind,
        title: &str,
        body: &str,
        cx: &mut Context<Self>,
    ) {
        cx.emit(AgentPaneEvent::Lifecycle(AgentEvent {
            route: self.agent_route.clone(),
            agent: self.kind.wire().to_string(),
            session_id: format!("agent-tab-{}", self.session_epoch),
            turn_id: (kind != AgentEventKind::SessionStarted)
                .then(|| format!("turn-{}", self.turn_seq)),
            kind,
            title: normalize_title(title),
            body: normalize_body(body),
        }));
    }

    fn latest_agent_message(&self) -> Option<&str> {
        self.items.iter().rev().find_map(|entry| match &entry.item {
            ChatItem::Agent { text, .. }
                if entry.turn == self.turn_seq && !text.trim().is_empty() =>
            {
                Some(text.as_str())
            }
            _ => None,
        })
    }

    /// Spawn the backend process (optionally resuming a persisted Claude
    /// session) and pump its messages onto the UI thread. Channel closure is
    /// the EOF signal (the sender is owned by the reader thread). Does not
    /// notify — callers decide whether a repaint is due.
    fn start_session(&mut self, resume: Option<String>, cx: &mut Context<Self>) {
        let kind = self.kind;
        let name = kind.display();
        let cwd = self.cwd.clone();

        self.seed_thread_defaults = resume.is_none();

        // Replacing a conversation must clear any running or unread state
        // associated with the previous backend before the new epoch can emit.
        cx.emit(AgentPaneEvent::Interrupted);
        self.session_epoch = next_session_epoch(self.session_epoch);
        self.skill_catalog = None;
        self.skill_binding = None;
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
                        cx.emit(AgentPaneEvent::Interrupted);
                        this.status = Status::Exited;
                        this.awaiting_command_turn = false;
                        if !this.command_queue.is_empty() {
                            this.command_queue.clear();
                            this.set_command_feedback(
                                CommandFeedbackKind::Error,
                                format!("Queued commands were cancelled because {name} exited."),
                                cx,
                            );
                        }
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
                cx.emit(AgentPaneEvent::Interrupted);
                self.status = Status::Exited;
                self.awaiting_command_turn = false;
                self.command_queue.clear();
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

    /// While tail-follow is engaged the list snaps to the end on every
    /// layout; the list disengages it on upward scroll and re-engages when
    /// the user returns to the bottom, so the follow state IS the
    /// at-the-bottom signal.
    fn transcript_is_at_bottom(&self) -> bool {
        self.transcript_list.is_following_tail()
    }

    /// Re-engaging tail mode also scrolls to the very end (past the last
    /// item), which stays correct while the last row is still growing.
    fn scroll_transcript_to_bottom(&self) {
        self.transcript_list.set_follow_mode(FollowMode::Tail);
    }

    fn push(&mut self, item: ChatItem, cx: &mut Context<Self>) {
        // Submitting a user message explicitly returns to the live tail.
        // Agent output preserves a manually chosen reading position via the
        // list's own tail-follow state.
        if matches!(&item, ChatItem::User { .. }) {
            self.scroll_transcript_to_bottom();
        }

        self.items.push(Entry {
            at: Local::now().format("%H:%M").to_string(),
            turn: self.turn_seq,
            item,
        });
        cx.notify();
    }

    fn send_user_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().to_string();

        if parse_slash_command(&text).is_some() {
            self.submit_current_slash(window, cx);
            return;
        }

        let text = text.trim().to_string();

        if text.is_empty() {
            return;
        }

        reconcile_skill_binding(&text, &mut self.skill_binding);
        let skill = if self.kind == AgentKind::Codex {
            match validate_skill_binding(
                &text,
                self.skill_binding.as_ref(),
                self.skill_catalog.as_ref(),
            ) {
                Ok(skill) => skill,
                Err(message) => {
                    self.set_command_feedback(CommandFeedbackKind::Error, message, cx);
                    return;
                }
            }
        } else {
            None
        };

        if self.send_text_with_skill(text, skill.as_ref(), cx) {
            self.skill_binding = None;
            self.input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    /// Send one user message through the session with full turn bookkeeping;
    /// also used for UI-generated messages such as the `/effort` command.
    /// Returns false when the session isn't ready yet.
    fn send_text(&mut self, text: String, cx: &mut Context<Self>) -> bool {
        self.send_text_with_skill(text, None, cx)
    }

    fn send_text_with_skill(
        &mut self,
        text: String,
        skill: Option<&SkillReference>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.awaiting_command_turn {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                "A command is starting; wait for its turn to begin.".to_string(),
                cx,
            );
            return false;
        }

        let settings = self.settings.clone();
        let outcome = match self.session.as_mut() {
            Some(session) => session.send_user_message(&text, &settings, skill),
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

    fn submit_current_slash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.input.read(cx).text().to_string();

        if self.submit_slash_input(&input, cx) {
            self.input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.palette_dismissed = false;
            self.palette_selected = 0;
        }
    }

    /// Route a leading slash before ordinary message handling. Every failure
    /// returns false so the user's input stays available for correction.
    fn submit_slash_input(&mut self, input: &str, cx: &mut Context<Self>) -> bool {
        let Some(parsed) = parse_slash_command(input) else {
            return false;
        };
        if parsed.name.is_empty() {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                "Choose a slash command from the list.".to_string(),
                cx,
            );
            return false;
        }

        let Some(command) = self
            .command_catalog()
            .into_iter()
            .find(|command| command.name == parsed.name)
        else {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                format!("Unknown command: /{}", parsed.name),
                cx,
            );
            return false;
        };

        // `/skills` owns a picker stage. A selected row rewrites the
        // composer to `$name`; the slash input itself is never a provider
        // command or an ordinary user turn.
        if command.arguments == SlashCommandArguments::Skills {
            let message = match self.skill_catalog.as_ref() {
                None => "Codex skill discovery is still loading.".to_string(),
                Some(catalog) if catalog.skills.is_empty() && !catalog.errors.is_empty() => {
                    catalog.errors[0].clone()
                }
                Some(catalog) if catalog.skills.is_empty() => {
                    "No Codex skills are available for this folder.".to_string()
                }
                Some(_) => "Choose a skill from the list.".to_string(),
            };

            self.set_command_feedback(CommandFeedbackKind::Error, message, cx);
            return false;
        }

        if command.arguments == SlashCommandArguments::None && !parsed.arguments.trim().is_empty() {
            self.set_command_feedback(
                CommandFeedbackKind::Error,
                format!("/{} does not accept arguments.", command.name),
                cx,
            );
            return false;
        }

        if command.arguments == SlashCommandArguments::Choices {
            if parsed.arguments.trim().is_empty() {
                self.set_command_feedback(
                    CommandFeedbackKind::Error,
                    format!("Choose a value for /{}.", command.name),
                    cx,
                );
                return false;
            }

            let choices = self.command_choices(&command.name);
            match resolve_choice(&parsed.arguments, &choices) {
                Ok(value) if command.name == "model" => {
                    self.settings.model = Some(value.clone());
                    self.remember_thread_defaults(cx);
                    self.set_command_feedback(
                        CommandFeedbackKind::Notice,
                        format!("Model set to {value}."),
                        cx,
                    );
                    return true;
                }
                Ok(value) if command.name == "permissions" => {
                    self.settings.approval = Some(value.clone());
                    self.remember_thread_defaults(cx);
                    self.set_command_feedback(
                        CommandFeedbackKind::Notice,
                        format!("Permissions set to {value}."),
                        cx,
                    );
                    return true;
                }
                Ok(_) => {}
                Err(message) => {
                    self.set_command_feedback(CommandFeedbackKind::Error, message, cx);
                    return false;
                }
            }
        }

        match command.name.as_str() {
            "new" | "clear" => {
                if self.is_command_busy() {
                    self.set_command_feedback(
                        CommandFeedbackKind::Error,
                        format!(
                            "/{} is available only while the agent is idle.",
                            command.name
                        ),
                        cx,
                    );
                    false
                } else {
                    self.reset_conversation(cx);
                    true
                }
            }
            "status" => {
                self.show_status(cx);
                true
            }
            "model" | "permissions" => false,
            _ => self.route_backend_command(
                PendingSlashCommand {
                    name: command.name,
                    arguments: parsed.arguments,
                },
                command.run_policy,
                cx,
            ),
        }
    }

    fn route_backend_command(
        &mut self,
        command: PendingSlashCommand,
        policy: SlashCommandRunPolicy,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.is_command_busy() {
            return match policy {
                SlashCommandRunPolicy::QueueUntilIdle => {
                    let name = command.name.clone();
                    self.command_queue.push_back(command);
                    self.set_command_feedback(
                        CommandFeedbackKind::Queued,
                        format!(
                            "Queued /{name} ({} command{} waiting).",
                            self.command_queue.len(),
                            if self.command_queue.len() == 1 {
                                ""
                            } else {
                                "s"
                            }
                        ),
                        cx,
                    );
                    true
                }
                SlashCommandRunPolicy::IdleOnly => {
                    self.set_command_feedback(
                        CommandFeedbackKind::Error,
                        format!(
                            "/{} is available only while the agent is idle.",
                            command.name
                        ),
                        cx,
                    );
                    false
                }
                SlashCommandRunPolicy::Immediate => self.execute_backend_command(command, cx),
            };
        }

        self.execute_backend_command(command, cx)
    }

    fn execute_backend_command(
        &mut self,
        command: PendingSlashCommand,
        cx: &mut Context<Self>,
    ) -> bool {
        let outcome = match self.session.as_mut() {
            Some(session) => session.execute_slash_command(&command.name, &command.arguments),
            None => SlashCommandOutcome::NotReady,
        };

        match outcome {
            SlashCommandOutcome::Accepted => {
                self.history_dismissed = true;
                self.awaiting_command_turn = true;
                self.set_command_feedback(
                    CommandFeedbackKind::Notice,
                    format!("Starting /{}…", command.name),
                    cx,
                );
                true
            }
            SlashCommandOutcome::Completed { message } => {
                self.set_command_feedback(
                    CommandFeedbackKind::Notice,
                    message.unwrap_or_else(|| format!("/{} completed.", command.name)),
                    cx,
                );
                true
            }
            SlashCommandOutcome::Rejected { message } => {
                self.set_command_feedback(CommandFeedbackKind::Error, message, cx);
                false
            }
            SlashCommandOutcome::NotReady => {
                self.set_command_feedback(
                    CommandFeedbackKind::Error,
                    format!(
                        "{} is still starting; try again in a moment.",
                        self.kind.display()
                    ),
                    cx,
                );
                false
            }
        }
    }

    fn run_next_queued_command(&mut self, cx: &mut Context<Self>) {
        if self.is_command_busy() {
            return;
        }
        let Some(command) = self.command_queue.pop_front() else {
            return;
        };

        if !self.execute_backend_command(command, cx) {
            self.command_queue.clear();
        }
    }

    fn reset_conversation(&mut self, cx: &mut Context<Self>) {
        self.session = None;
        self.items.clear();
        // A fresh conversation always follows the live tail again, even if
        // the previous transcript was scrolled up when it was discarded.
        self.scroll_transcript_to_bottom();
        self.settings = ThreadSettings::default();
        self.models.clear();
        self.expanded_groups.clear();
        self.expanded_turns.clear();
        self.expanded_rows.clear();
        self.turn_seq = 0;
        self.working_started = None;
        self.skill_catalog = None;
        self.skill_binding = None;
        reset_command_runtime(
            self.kind == AgentKind::Codex,
            &mut self.pending_approval,
            &mut self.provider_commands,
            &mut self.provider_commands_ready,
            &mut self.command_queue,
            &mut self.awaiting_command_turn,
            &mut self.palette_selected,
            &mut self.palette_dismissed,
        );
        self.command_feedback = None;

        // History records belong to the provider and remain intact; only the
        // live backend and this tab's conversation presentation are reset.
        self.start_session(None, cx);
    }

    fn show_status(&mut self, cx: &mut Context<Self>) {
        let status = match self.status {
            Status::Starting => "starting",
            Status::Idle => "idle",
            Status::Running => "running",
            Status::Exited => "exited",
        };
        let mut fields = vec![
            format!("backend={}", self.kind.display()),
            format!("status={status}"),
        ];

        for (name, value) in [
            ("model", self.settings.model.as_deref()),
            ("permissions", self.settings.approval.as_deref()),
            ("sandbox", self.settings.sandbox.as_deref()),
            ("effort", self.settings.effort.as_deref()),
            ("tier", self.settings.tier.as_deref()),
        ] {
            if let Some(value) = value {
                fields.push(format!("{name}={value}"));
            }
        }
        if !self.command_queue.is_empty() {
            fields.push(format!("queued={}", self.command_queue.len()));
        }

        self.set_command_feedback(CommandFeedbackKind::Notice, fields.join(" · "), cx);
    }

    fn set_command_feedback(
        &mut self,
        kind: CommandFeedbackKind,
        message: String,
        cx: &mut Context<Self>,
    ) {
        self.command_feedback = Some(CommandFeedback { kind, message });
        cx.notify();
    }

    fn is_command_busy(&self) -> bool {
        self.status == Status::Running || self.awaiting_command_turn
    }

    fn skill_disabled_reason(&self, skill: &SkillInfo) -> Option<String> {
        if !skill.enabled {
            Some("Disabled by Codex".to_string())
        } else if matches!(self.status, Status::Starting | Status::Exited) {
            Some(match self.status {
                Status::Starting => "Agent is still starting".to_string(),
                Status::Exited => "Agent has exited".to_string(),
                _ => unreachable!(),
            })
        } else {
            None
        }
    }

    fn command_catalog(&self) -> Vec<SlashCommandInfo> {
        let adapter = self
            .session
            .as_ref()
            .map(Backend::adapter_commands)
            .unwrap_or_else(|| match self.kind {
                AgentKind::Codex => app_server::Session::adapter_commands(),
                AgentKind::Claude => stream_json::Session::adapter_commands(),
            });

        merge_catalog(local_commands(), adapter, self.provider_commands.clone())
    }

    fn command_choices(&self, command: &str) -> Vec<(String, String)> {
        match command {
            "model" => self
                .models
                .iter()
                .map(|model| (model.model.clone(), model.display.clone()))
                .collect(),
            "permissions" => match self.kind {
                AgentKind::Codex => app_server::APPROVAL_OPTIONS
                    .iter()
                    .map(|value| (value.to_string(), value.to_string()))
                    .collect(),
                AgentKind::Claude => stream_json::PERMISSION_OPTIONS
                    .iter()
                    .map(|value| (value.to_string(), value.to_string()))
                    .collect(),
            },
            _ => Vec::new(),
        }
    }

    fn palette_model(&self, cx: &Context<Self>) -> Option<PaletteModel> {
        if self.palette_dismissed {
            return None;
        }

        let input = self.input.read(cx);
        let text = input.text().to_string();
        let parsed = parse_slash_command(&text)?;
        let cursor = input.cursor();
        let catalog = self.command_catalog();

        if parsed.has_argument_separator {
            let command = catalog.iter().find(|command| command.name == parsed.name)?;

            if command.arguments == SlashCommandArguments::Skills {
                let query = parsed.arguments.trim().to_ascii_lowercase();
                let Some(skill_catalog) = self.skill_catalog.as_ref() else {
                    return Some(PaletteModel {
                        rows: Vec::new(),
                        note: Some("Codex skill discovery is still loading".to_string()),
                    });
                };
                let rows = filter_skill_catalog(&skill_catalog.skills, &query)
                    .into_iter()
                    .map(|skill| {
                        let disabled_reason = self.skill_disabled_reason(&skill);

                        PaletteRow {
                            label: format!("${}", skill.name),
                            description: skill.description.clone(),
                            hint: Some(skill.scope.clone()),
                            disabled_reason,
                            action: PaletteAction::Skill(skill),
                        }
                    })
                    .collect::<Vec<_>>();
                let note = if rows.is_empty() && !skill_catalog.errors.is_empty() {
                    Some(skill_catalog.errors[0].clone())
                } else if rows.is_empty() && query.is_empty() {
                    Some("No Codex skills are available for this folder".to_string())
                } else if rows.is_empty() {
                    Some("No matching skills".to_string())
                } else if let Some(error) = skill_catalog.errors.first() {
                    Some(format!("Some skills could not be loaded: {error}"))
                } else {
                    None
                };

                return Some(PaletteModel { rows, note });
            }

            if command.arguments != SlashCommandArguments::Choices {
                return None;
            }

            let query = parsed.arguments.to_ascii_lowercase();
            let rows = self
                .command_choices(&command.name)
                .into_iter()
                .filter(|(value, label)| {
                    query.is_empty()
                        || value.to_ascii_lowercase().contains(&query)
                        || label.to_ascii_lowercase().contains(&query)
                })
                .map(|(value, label)| PaletteRow {
                    description: value.clone(),
                    label,
                    hint: None,
                    disabled_reason: None,
                    action: PaletteAction::Choice {
                        command: command.name.clone(),
                        value,
                    },
                })
                .collect::<Vec<_>>();

            return Some(PaletteModel {
                note: rows.is_empty().then(|| "No matching values".to_string()),
                rows,
            });
        }

        // Moving the caret into later prose must not turn an ordinary edit
        // into palette navigation; only the first slash token owns the keys.
        if cursor > 1 + parsed.name.len() {
            return None;
        }

        let skills: &[SkillInfo] = if self.kind == AgentKind::Codex {
            self.skill_catalog
                .as_ref()
                .map(|catalog| catalog.skills.as_slice())
                .unwrap_or_default()
        } else {
            &[]
        };
        let rows = filter_palette_catalog(&catalog, skills, &parsed.name)
            .into_iter()
            .map(|entry| match entry {
                PaletteCatalogEntry::Command(command) => {
                    let disabled_reason = if command.run_policy == SlashCommandRunPolicy::IdleOnly
                        && self.is_command_busy()
                    {
                        Some("Available when the agent is idle".to_string())
                    } else if command.source != SlashCommandSource::Local
                        && matches!(self.status, Status::Starting | Status::Exited)
                    {
                        Some(match self.status {
                            Status::Starting => "Agent is still starting".to_string(),
                            Status::Exited => "Agent has exited".to_string(),
                            _ => unreachable!(),
                        })
                    } else {
                        None
                    };

                    PaletteRow {
                        label: format!("/{}", command.name),
                        description: command.description.clone(),
                        hint: command.argument_hint.clone(),
                        disabled_reason,
                        action: PaletteAction::Command(command),
                    }
                }
                PaletteCatalogEntry::Skill(skill) => PaletteRow {
                    label: format!("/{}", skill.name),
                    description: skill.description.clone(),
                    hint: Some(format!("skill · {}", skill.scope)),
                    disabled_reason: self.skill_disabled_reason(&skill),
                    action: PaletteAction::Skill(skill),
                },
            })
            .collect::<Vec<_>>();
        let note = if rows.is_empty() {
            if self.kind == AgentKind::Codex && self.skill_catalog.is_none() {
                Some("Codex skill discovery is still loading".to_string())
            } else if self.kind == AgentKind::Codex
                && self
                    .skill_catalog
                    .as_ref()
                    .is_some_and(|catalog| !catalog.errors.is_empty())
            {
                self.skill_catalog
                    .as_ref()
                    .and_then(|catalog| catalog.errors.first().cloned())
            } else if self.kind == AgentKind::Codex {
                Some("No matching commands or skills".to_string())
            } else {
                Some("No matching commands".to_string())
            }
        } else if self.kind == AgentKind::Claude && !self.provider_commands_ready {
            Some("Claude command discovery is still loading".to_string())
        } else if self.kind == AgentKind::Codex && self.skill_catalog.is_none() {
            Some("Codex skill discovery is still loading".to_string())
        } else if self.kind == AgentKind::Codex {
            self.skill_catalog
                .as_ref()
                .and_then(|catalog| catalog.errors.first())
                .map(|error| format!("Some skills could not be loaded: {error}"))
        } else {
            None
        };

        Some(PaletteModel { rows, note })
    }

    fn handle_palette_control(
        &mut self,
        control: PaletteControl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.palette_model(cx) else {
            cx.propagate();
            return;
        };

        cx.stop_propagation();

        match control {
            PaletteControl::Previous | PaletteControl::Next => {
                let direction = match control {
                    PaletteControl::Previous => PaletteDirection::Previous,
                    PaletteControl::Next => PaletteDirection::Next,
                    _ => unreachable!(),
                };

                if let Some(selected) =
                    move_palette_selection(self.palette_selected, model.rows.len(), direction)
                {
                    self.palette_selected = selected;
                    self.palette_scroll.scroll_to_item(self.palette_selected);
                    cx.notify();
                }
            }
            PaletteControl::Activate => {
                if model.rows.is_empty() {
                    self.submit_current_slash(window, cx);
                } else {
                    self.activate_palette_index(self.palette_selected, true, window, cx);
                }
            }
            PaletteControl::Complete => {
                self.activate_palette_index(self.palette_selected, false, window, cx);
            }
            PaletteControl::Dismiss => {
                self.palette_dismissed = true;
                cx.notify();
            }
        }
    }

    fn activate_palette_index(
        &mut self,
        index: usize,
        execute: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self
            .palette_model(cx)
            .and_then(|model| model.rows.get(index).cloned())
        else {
            return;
        };

        if let Some(reason) = row.disabled_reason {
            self.set_command_feedback(CommandFeedbackKind::Error, reason, cx);
            return;
        }

        let (text, can_execute) = match row.action {
            PaletteAction::Command(command) => {
                let needs_arguments = command.arguments != SlashCommandArguments::None;
                (
                    format!(
                        "/{}{}",
                        command.name,
                        if needs_arguments { " " } else { "" }
                    ),
                    !needs_arguments,
                )
            }
            PaletteAction::Choice { command, value } => (format!("/{command} {value}"), true),
            PaletteAction::Skill(skill) => {
                let Ok((text, binding)) = prepare_skill_selection(&skill) else {
                    self.set_command_feedback(
                        CommandFeedbackKind::Error,
                        format!("${} is disabled by Codex.", skill.name),
                        cx,
                    );
                    return;
                };

                self.input.update(cx, |input, cx| {
                    input.set_value(text.clone(), window, cx);
                    input.set_selected_range(text.len()..text.len(), cx);
                });
                self.skill_binding = Some(binding);
                self.palette_selected = 0;
                self.palette_dismissed = true;
                cx.notify();
                return;
            }
        };

        self.input.update(cx, |input, cx| {
            input.set_value(text.clone(), window, cx);
            input.set_selected_range(text.len()..text.len(), cx);
        });
        self.palette_selected = 0;

        if execute && can_execute {
            self.submit_current_slash(window, cx);
        } else {
            cx.notify();
        }
    }

    /// Start the turn clock and drive the once-a-second repaint of the live
    /// progress row; the ticker stops itself once `finish_working` clears it.
    fn start_working(&mut self, cx: &mut Context<Self>) {
        self.working_started = Some(Instant::now());
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
            cx.emit(AgentPaneEvent::Interrupted);
            cx.notify();
        }
    }

    fn respond_approval(&mut self, decision: &str, cx: &mut Context<Self>) {
        // The card is dismissed immediately for a snappy UI; the session's
        // `ApprovalResolved` confirmation is then an idempotent status refresh.
        self.pending_approval = None;
        self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);

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
                // Ready can fire again mid-session (Claude's first-turn init
                // confirms the permission mode); a payload without effort
                // keeps the user's pick — Claude never reports effort, so
                // None there means "unknown", never "reset".
                let effort = settings.effort.clone().or(self.settings.effort.take());
                let mut next = ThreadSettings { effort, ..settings };

                // Fresh conversations seed from the remembered per-kind picks
                // (a remembered value wins over the CLI default); resumed
                // threads and later Ready confirmations take the wire as-is.
                if take(&mut self.seed_thread_defaults)
                    && let Some(stored) = cx
                        .try_global::<AgentThreadDefaults>()
                        .and_then(|defaults| defaults.0.get(self.kind.wire()))
                {
                    next = ThreadSettings {
                        model: stored.model.clone().or(next.model),
                        approval: stored.approval.clone().or(next.approval),
                        sandbox: stored.sandbox.clone().or(next.sandbox),
                        effort: stored.effort.clone().or(next.effort),
                        tier: stored.tier.clone().or(next.tier),
                    };
                }

                self.settings = next;
                // Claude's first-turn init confirms settings after its
                // synthetic TurnStarted event; that confirmation must not
                // make an active turn look idle and admit overlapping work.
                if self.status != Status::Running {
                    self.status = Status::Idle;
                }
                cx.notify();
            }
            SessionEvent::Models(models) => {
                self.models = models;
                cx.notify();
            }
            SessionEvent::Commands(commands) => {
                self.provider_commands = commands;
                self.provider_commands_ready = true;
                self.palette_selected = 0;
                cx.notify();
            }
            SessionEvent::Skills(catalog) => {
                self.skill_catalog = Some(catalog);
                self.palette_selected = 0;
                cx.notify();
            }
            SessionEvent::SlashCommandResult { name, outcome } => match outcome {
                SlashCommandOutcome::Accepted => {
                    self.set_command_feedback(
                        CommandFeedbackKind::Notice,
                        format!("/{name} accepted."),
                        cx,
                    );
                }
                SlashCommandOutcome::Completed { message } => {
                    self.set_command_feedback(
                        CommandFeedbackKind::Notice,
                        message.unwrap_or_else(|| format!("/{name} completed.")),
                        cx,
                    );
                    if self.awaiting_command_turn && self.status != Status::Running {
                        self.awaiting_command_turn = false;
                        self.run_next_queued_command(cx);
                    }
                }
                SlashCommandOutcome::Rejected { message } => {
                    self.awaiting_command_turn = false;
                    self.set_command_feedback(CommandFeedbackKind::Error, message, cx);
                    self.run_next_queued_command(cx);
                }
                SlashCommandOutcome::NotReady => {
                    self.awaiting_command_turn = false;
                    self.set_command_feedback(
                        CommandFeedbackKind::Error,
                        format!("{} is not ready.", self.kind.display()),
                        cx,
                    );
                    self.run_next_queued_command(cx);
                }
            },
            SessionEvent::TurnStarted => {
                if claim_command_turn_start(&mut self.awaiting_command_turn) {
                    self.turn_seq += 1;
                    self.start_working(cx);
                }
                self.status = Status::Running;
                self.emit_lifecycle(AgentEventKind::PromptSubmitted, "", "", cx);
                cx.notify();
            }
            SessionEvent::TurnCompleted { error } => {
                let completion_body = error
                    .clone()
                    .or_else(|| self.latest_agent_message().map(str::to_owned))
                    .unwrap_or_else(|| format!("{} completed the turn", self.kind.display()));
                self.awaiting_command_turn = false;
                self.finish_working(cx);
                self.refresh_git_branch(cx);
                if self.status == Status::Running {
                    self.status = Status::Idle;
                }
                if let Some(text) = error {
                    self.push(ChatItem::Error { text }, cx);
                }
                self.emit_lifecycle(
                    AgentEventKind::Stopped,
                    &format!("{} finished", self.kind.display()),
                    &completion_body,
                    cx,
                );
                self.run_next_queued_command(cx);
                cx.notify();
            }
            SessionEvent::ContextWindowUpdated(usage) => {
                self.context_window_usage = Some(usage);
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
                self.emit_lifecycle(
                    AgentEventKind::PermissionRequested,
                    &format!("{} needs input", self.kind.display()),
                    &description,
                    cx,
                );
                self.pending_approval = Some(description);
                cx.notify();
            }
            SessionEvent::ApprovalResolved => {
                self.pending_approval = None;
                self.emit_lifecycle(AgentEventKind::ToolFinished, "", "", cx);
                cx.notify();
            }
            SessionEvent::Error { message, fatal } => {
                let cancelled_queue = fatal && !self.command_queue.is_empty();
                if fatal {
                    cx.emit(AgentPaneEvent::Interrupted);
                    self.status = Status::Exited;
                    self.awaiting_command_turn = false;
                    self.command_queue.clear();
                } else if self.awaiting_command_turn {
                    self.awaiting_command_turn = false;
                }
                self.push(ChatItem::Error { text: message }, cx);
                if cancelled_queue {
                    self.set_command_feedback(
                        CommandFeedbackKind::Error,
                        "Queued commands were cancelled because the session failed.".to_string(),
                        cx,
                    );
                } else if !fatal {
                    self.run_next_queued_command(cx);
                }
            }
            SessionEvent::History(sessions) => {
                // Pages accumulate: the first page lands in an empty list,
                // later cursor pages extend it. A /new backend may publish
                // the first page again, so ids are deduplicated in place.
                for session in sessions {
                    if !self
                        .history
                        .iter()
                        .any(|existing| existing.id == session.id)
                    {
                        self.history.push(session);
                    }
                }
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
                    output: None,
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
        // The resumed thread's own settings are authoritative; the remembered
        // per-kind defaults must not overwrite them on the next Ready.
        self.seed_thread_defaults = false;

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
            SessionItem::FileChange {
                id,
                paths,
                diff,
                status,
            } => ChatItem::FileChange {
                item_id: id,
                summary: paths,
                diff,
                status: status.unwrap_or_else(|| "inProgress".to_string()),
            },
            SessionItem::Other {
                id,
                kind,
                title,
                output,
                status,
            } => ChatItem::Tool {
                item_id: id,
                kind,
                title,
                output,
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
                    ChatItem::FileChange { status, diff, .. },
                    SessionItem::FileChange {
                        status: new_status,
                        diff: new_diff,
                        ..
                    },
                ) => {
                    if let Some(state) = new_status {
                        *status = state.clone();
                    }
                    if new_diff.is_some() {
                        *diff = new_diff.clone();
                    }
                }
                (
                    ChatItem::Tool { status, output, .. },
                    SessionItem::Other {
                        status: new_status,
                        output: new_output,
                        ..
                    },
                ) => {
                    if let Some(state) = new_status {
                        *status = state.clone();
                    }
                    if new_output.is_some() {
                        *output = new_output.clone();
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

        cx.notify();
    }

    /// Remember the pane's current thread settings as the defaults for future
    /// conversations of this agent kind. Called after every user-driven
    /// settings change (dropdowns and slash commands).
    fn remember_thread_defaults(&self, cx: &mut Context<Self>) {
        cx.default_global::<AgentThreadDefaults>()
            .0
            .insert(self.kind.wire().to_string(), self.settings.clone());
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
                break;
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

/// Collapsed head of an oversized user prompt, cut at a line boundary, or
/// `None` when the whole prompt fits under the caps. Byte-capped scanning
/// keeps this O(cap) per call regardless of paste size, so calling it every
/// frame for a visible row stays cheap.
fn truncated_user_prompt(text: &str) -> Option<&str> {
    const MAX_LINES: usize = 3;
    const MAX_BYTES: usize = 4096;

    if text.len() <= MAX_BYTES && line_count_at_most(text, MAX_LINES) {
        return None;
    }

    let mut end = 0;
    let mut lines = 0;

    for line in text.split_inclusive('\n') {
        if lines == MAX_LINES || end + line.len() > MAX_BYTES {
            if end == 0 {
                // A single line longer than the byte cap: cut inside it at a
                // character boundary.
                let cut = text
                    .char_indices()
                    .map(|(i, _)| i)
                    .take_while(|i| *i <= MAX_BYTES)
                    .last()
                    .unwrap_or(0);
                return Some(&text[..cut]);
            }
            return Some(&text[..end]);
        }
        end += line.len();
        lines += 1;
    }

    None
}

/// Whether `text` has at most `max` lines, scanning no further than needed.
fn line_count_at_most(text: &str, max: usize) -> bool {
    text.split_inclusive('\n').take(max + 1).count() <= max
}

/// Wrap tool output in a Markdown fence so the transcript's TextView pipeline
/// renders it as a syntax-highlighted code block. The fence grows past any
/// backtick run in the body, so raw output can never terminate the block
/// early.
fn fenced_code_block(output: &str) -> String {
    fenced_code_block_as(output, detect_output_language(output))
}

/// Same fence-growing wrap with an explicit language tag, for content whose
/// format is known from context (e.g. edit diffs) rather than sniffed.
fn fenced_code_block_as(output: &str, lang: &str) -> String {
    let mut fence = String::from("```");
    while output.contains(fence.as_str()) {
        fence.push('`');
    }
    format!("{fence}{lang}\n{output}\n{fence}")
}

/// First-bytes sniff covering the two formats tool output actually produces
/// in bulk (diffs and JSON payloads); anything else renders as an unhighlighted
/// code block. Extend per-tool only if more grammars earn their keep.
fn detect_output_language(output: &str) -> &'static str {
    let trimmed = output.trim_start();
    if trimmed.starts_with("diff --git") || trimmed.starts_with("@@ ") {
        "diff"
    } else if trimmed.starts_with('{') || trimmed.starts_with('[') {
        "json"
    } else {
        ""
    }
}

/// Claude's Read tool returns cat -n style lines ("   12→text"); strip the
/// gutter so the source underneath highlights as its own language. Any line
/// without the gutter leaves the text untouched (format drift safety).
fn strip_read_gutter(output: &str) -> Option<String> {
    let mut body = String::with_capacity(output.len());

    for line in output.lines() {
        let (gutter, text) = line.split_once('→')?;
        let number = gutter.trim_start();
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        body.push_str(text);
        body.push('\n');
    }

    (!body.is_empty()).then_some(body)
}

/// Fence-language tag for a file path. The highlight registry accepts file
/// extensions as language aliases (rs, py, yml, …) and resolves unknown ones
/// to plain, so the extension itself is the tag.
fn file_extension_lang(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
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
            summary,
            diff,
            status,
            ..
        } => match diff {
            Some(diff) => format!("Edit {summary} — {status}\n{diff}"),
            None => format!("Edit {summary} — {status}"),
        },
        ChatItem::Tool {
            kind,
            title,
            output,
            status,
            ..
        } => match output {
            Some(output) => format!("{kind} {title} — {status}\n{output}"),
            None => format!("{kind} {title} — {status}"),
        },
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

fn compact_token_count(tokens: u64) -> String {
    match tokens {
        0..=999 => tokens.to_string(),
        1_000..=9_999 => format!("{:.1}k", tokens as f64 / 1_000.0).replace(".0k", "k"),
        10_000..=999_999 => format!("{}k", tokens / 1_000),
        _ => format!("{:.1}m", tokens as f64 / 1_000_000.0).replace(".0m", "m"),
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

/// One transcript row for the virtualized list. `PartialEq` powers the
/// render-time diff: kind + indices catch structural changes (fold, collapse,
/// appended rows), the fingerprint catches in-place content changes that move
/// a row's height (streamed text, status flips, detail expansion).
#[derive(Clone, PartialEq, Eq)]
enum RowSpec {
    Entry {
        index: usize,
        fingerprint: u64,
    },
    Work {
        index: usize,
        fingerprint: u64,
    },
    FoldHeader {
        turn: u64,
        seconds: u64,
        folded: bool,
    },
    RunToggle {
        run_start: usize,
        hidden_count: usize,
        expanded: bool,
    },
    Working,
}

/// Height-relevant signature of a transcript entry. Lengths and small fields
/// instead of hashing full text: O(1) per row per frame, and every real
/// mutation (streamed append, status transition, exit code, expansion) moves
/// at least one component.
fn entry_fingerprint(item: &ChatItem, detail_expanded: bool) -> u64 {
    let (content_len, status_len, extra) = match item {
        ChatItem::User { text }
        | ChatItem::Agent { text, .. }
        | ChatItem::Reasoning { text, .. }
        | ChatItem::Error { text } => (text.len(), 0, 0),
        ChatItem::Command {
            command,
            output,
            status,
            exit_code,
            ..
        } => (
            command.len() + output.len(),
            status.len(),
            exit_code.map_or(0, |code| (code as u64) ^ (1 << 20)),
        ),
        ChatItem::FileChange {
            summary,
            diff,
            status,
            ..
        } => (
            summary.len() + diff.as_ref().map_or(0, String::len),
            status.len(),
            0,
        ),
        ChatItem::Tool {
            kind,
            title,
            output,
            status,
            ..
        } => (
            kind.len() + title.len() + output.as_ref().map_or(0, String::len),
            status.len(),
            0,
        ),
        ChatItem::Working { done_seconds, .. } => (done_seconds.unwrap_or(0) as usize, 0, 0),
    };

    (content_len as u64)
        ^ ((status_len as u64) << 48)
        ^ (extra << 24)
        ^ ((detail_expanded as u64) << 63)
}

impl gpui::EventEmitter<AgentPaneEvent> for AgentPane {}

impl gpui::Focusable for AgentPane {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AgentPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let collapse = cx.global::<AppSettings>().collapse_tool_calls;
        let command_palette = self.render_command_palette(cx);
        let command_feedback = self.command_feedback.as_ref().map(|feedback| {
            let (color, label) = match feedback.kind {
                CommandFeedbackKind::Notice => (cx.theme().primary, "NOTICE"),
                CommandFeedbackKind::Error => (cx.theme().danger, "ERROR"),
                CommandFeedbackKind::Queued => (cx.theme().warning, "QUEUED"),
            };

            h_flex()
                .w_full()
                .gap_2()
                .px_3()
                .pb_2()
                .text_xs()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(color)
                        .child(label),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_color(cx.theme().muted_foreground)
                        .child(feedback.message.clone()),
                )
        });

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
        let transcript_scrolled_up = !self.transcript_is_at_bottom();

        // The history list only makes sense while the tab is a blank slate:
        // no transcript yet and no conversation committed to. It shows as
        // placeholders once the count pass promises rows, then as real rows.
        let history_rows = self.history_pending.unwrap_or(self.history.len());
        let history = (self.items.is_empty() && history_rows > 0 && !self.history_dismissed)
            .then(|| self.render_history(cx));

        v_flex()
            .size_full()
            .track_focus(&self.focus)
            // Escape force-stops the agent whenever the pane or composer has
            // focus. The input propagates Escape here when the editor did not
            // consume it (inline completion, IME), and transcript clicks focus
            // the pane below. A pending approval is cancelled (deny +
            // interrupt), while a running turn is interrupted directly.
            .on_action(cx.listener(|this, _: &Escape, _, cx| {
                if this.pending_approval.is_some() {
                    this.respond_approval("cancel", cx);
                } else if this.status == Status::Running {
                    this.interrupt(cx);
                }
            }))
            // The agent tab is a terminal surface stand-in, so it overrides the
            // chrome's UI font with its own configured font (Settings → Agent
            // Font), same as the terminal pane does with the terminal font.
            .font_family(cx.global::<AppSettings>().agent_font_family.clone())
            .text_size(px(cx.global::<AppSettings>().agent_font_size as f32))
            .child(
                // The scrollbar must sit OUTSIDE the scrolling element (a
                // child would scroll away with the content), so a relative
                // wrapper hosts the scroll area and the overlay bar.
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("agent-transcript")
                            .size_full()
                            // Reading or selecting transcript content should
                            // leave Escape routed to the pane-level interrupt
                            // handler instead of whichever control previously
                            // held keyboard focus.
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    window.focus(&this.focus, cx);
                                }),
                            )
                            .child(
                                // Element callbacks run after this render's
                                // entity lease is released, so the row builder
                                // re-enters the pane through a weak handle.
                                list(self.transcript_list.clone(), {
                                    let this = cx.entity().downgrade();
                                    move |ix, _window, cx| {
                                        this.update(cx, |this, cx| this.render_row(ix, cx))
                                            .unwrap_or_else(|_| div().into_any_element())
                                    }
                                })
                                .size_full()
                                .pt_3(),
                            ),
                    )
                    // The bare Scrollbar element carries no inset of its own,
                    // so it lands at its static flow position (below the
                    // full-height sibling); the pinned strip gives it a
                    // deterministic containing block at the right edge.
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(16.))
                            .child(Scrollbar::vertical(&self.transcript_list)),
                    )
                    .when(transcript_scrolled_up, |this| {
                        this.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom(px(12.))
                                .flex()
                                .justify_center()
                                .child(
                                    Button::new("agent-jump-to-bottom")
                                        .outline()
                                        .small()
                                        .rounded(px(999.))
                                        .icon(IconName::ArrowDown)
                                        .tooltip("Jump to latest")
                                        .shadow_md()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.scroll_transcript_to_bottom();
                                            cx.notify();
                                        })),
                                ),
                        )
                    }),
            )
            .child({
                // Composer area: auxiliary strips sit outside the bordered,
                // shadowed shell on a deeper surface. History is absolutely
                // anchored above the shell because it only exists while the
                // transcript is empty; loading it must never participate in
                // composer height calculation. Both strips are painted before
                // the shell, whose edge and shadow keep them visibly tucked
                // behind the input card.
                div().w_full().px_3().pb_3().pt_1().child(
                    div()
                        .relative()
                        .pb(px(30.))
                        .children(history.map(|history| {
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom(relative(1.))
                                .mb(px(-14.))
                                .child(history)
                        }))
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .flex()
                                .justify_center()
                                .child(self.render_composer_status(cx)),
                        )
                        .child(
                            v_flex()
                                .w_full()
                                .rounded(px(12.))
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
                                        // GPUI resolves these keystrokes
                                        // into Input actions before raw
                                        // key listeners run. Capturing
                                        // the actions lets the palette
                                        // own navigation while visible;
                                        // the handler propagates them
                                        // unchanged when it is closed.
                                        .capture_action(cx.listener(
                                            |this, _: &MoveUp, window, cx| {
                                                this.handle_palette_control(
                                                    PaletteControl::Previous,
                                                    window,
                                                    cx,
                                                )
                                            },
                                        ))
                                        .capture_action(cx.listener(
                                            |this, _: &MoveDown, window, cx| {
                                                this.handle_palette_control(
                                                    PaletteControl::Next,
                                                    window,
                                                    cx,
                                                )
                                            },
                                        ))
                                        .capture_action(cx.listener(
                                            |this, action: &Enter, window, cx| {
                                                if action.shift || action.secondary {
                                                    cx.propagate();
                                                } else {
                                                    this.handle_palette_control(
                                                        PaletteControl::Activate,
                                                        window,
                                                        cx,
                                                    );
                                                }
                                            },
                                        ))
                                        .capture_action(cx.listener(
                                            |this, _: &IndentInline, window, cx| {
                                                this.handle_palette_control(
                                                    PaletteControl::Complete,
                                                    window,
                                                    cx,
                                                )
                                            },
                                        ))
                                        .capture_action(cx.listener(
                                            |this, _: &Escape, window, cx| {
                                                this.handle_palette_control(
                                                    PaletteControl::Dismiss,
                                                    window,
                                                    cx,
                                                )
                                            },
                                        ))
                                        // The prompt editor reads larger than the
                                        // chrome around it (t3code uses 16px over
                                        // a 14px UI); +2 keeps that ratio at any
                                        // configured agent font size.
                                        .text_size(px(cx.global::<AppSettings>().agent_font_size
                                            as f32
                                            + 2.0))
                                        .child(Input::new(&self.input).appearance(false)),
                                )
                                .children(command_feedback)
                                .child(
                                    h_flex()
                                        .w_full()
                                        .px_2p5()
                                        .pb_2p5()
                                        .pt_0p5()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .child(self.render_settings_row(cx)),
                                        )
                                        // Stop replaces Send in place while a
                                        // turn runs.
                                        .child(if running {
                                            Button::new("agent-send")
                                                .danger()
                                                .rounded(px(999.))
                                                .child(
                                                    div()
                                                        .size(px(10.))
                                                        .rounded_sm()
                                                        .bg(gpui::white()),
                                                )
                                                .on_click(
                                                    cx.listener(|this, _, _, cx| {
                                                        this.interrupt(cx)
                                                    }),
                                                )
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
                        .children(command_palette.map(|palette| {
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom(relative(1.))
                                .mb_2()
                                .occlude()
                                .child(palette)
                        })),
                )
            })
    }
}

impl AgentPane {
    fn render_command_palette(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let model = self.palette_model(cx)?;
        let selected = self
            .palette_selected
            .min(model.rows.len().saturating_sub(1));
        let rows = model
            .rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let disabled = row.disabled_reason.is_some();
                let detail = row.disabled_reason.clone().unwrap_or(row.description);
                let background = (index == selected).then(|| cx.theme().muted.opacity(0.7));

                div()
                    .id(("agent-slash-command", index))
                    .h(px(48.))
                    .flex_none()
                    .px_3()
                    .py_1p5()
                    .rounded(cx.theme().radius)
                    .when_some(background, |this, color| this.bg(color))
                    .when(disabled, |this| this.opacity(0.5))
                    .when(!disabled, |this| {
                        this.hover(|style| style.bg(cx.theme().muted.opacity(0.45)))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_palette_index(index, true, window, cx)
                    }))
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().foreground)
                                    .child(row.label),
                            )
                            .children(row.hint.map(|hint| {
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground.opacity(0.75))
                                    .child(hint)
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .text_color(cx.theme().muted_foreground)
                            .child(detail),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let note = model.note.map(|note| {
            div()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground.opacity(0.75))
                .child(note)
        });

        Some(
            v_flex()
                .id("agent-slash-command-palette")
                .w_full()
                .max_h(px(9. * 48. + 36.))
                .overflow_y_scroll()
                .track_scroll(&self.palette_scroll)
                .p_1()
                .rounded(px(12.))
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().popover)
                .shadow_lg()
                .children(rows)
                .children(note)
                .into_any_element(),
        )
    }

    /// Render one turn: user rows, then (for settled turns) a clickable
    /// "Worked for Ns" fold header hiding the intermediate work rows by
    /// default, then the final reply. Running turns render chronologically.
    fn entry_spec(&self, index: usize) -> RowSpec {
        RowSpec::Entry {
            index,
            fingerprint: entry_fingerprint(
                &self.items[index].item,
                self.expanded_rows.contains(&index),
            ),
        }
    }

    fn work_spec(&self, index: usize) -> RowSpec {
        RowSpec::Work {
            index,
            fingerprint: entry_fingerprint(
                &self.items[index].item,
                self.expanded_rows.contains(&index),
            ),
        }
    }

    /// Data-only description of every transcript row, in render order. This
    /// is the single source of truth for the transcript's structure; the
    /// virtualized list builds elements only for the visible slice of it.
    fn build_row_specs(&self, collapse: bool) -> Vec<RowSpec> {
        let mut rows = Vec::new();
        let mut start = 0;

        while start < self.items.len() {
            let turn = self.items[start].turn;
            let mut end = start + 1;

            while end < self.items.len() && self.items[end].turn == turn {
                end += 1;
            }

            self.turn_specs(turn, start, end, collapse, &mut rows);
            start = end;
        }

        // Live progress row, pinned below everything the running turn has
        // produced; replaced by the turn's fold header on completion.
        if self.working_started.is_some() {
            rows.push(RowSpec::Working);
        }

        rows
    }

    fn turn_specs(
        &self,
        turn: u64,
        start: usize,
        end: usize,
        collapse: bool,
        rows: &mut Vec<RowSpec>,
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
            self.stream_specs(start, end, &|_| false, collapse, rows);
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
                rows.push(self.entry_spec(i));
            }
        }

        rows.push(RowSpec::FoldHeader {
            turn,
            seconds,
            folded,
        });

        if folded {
            // Errors stay visible even inside a folded turn.
            for i in start..end {
                if matches!(&self.items[i].item, ChatItem::Error { .. }) {
                    rows.push(self.entry_spec(i));
                }
            }
        } else {
            let skip = |i: usize| {
                Some(i) == final_agent || matches!(&self.items[i].item, ChatItem::User { .. })
            };
            self.stream_specs(start, end, &skip, collapse, rows);
        }

        if let Some(i) = final_agent {
            rows.push(self.entry_spec(i));
        }
    }

    /// Chronological rows for a slice of the transcript, collapsing runs of
    /// consecutive work-log rows to the newest one plus a "+N previous tool
    /// calls" toggle (when the collapse setting is on). Hidden entries are
    /// transparent: they neither render nor split a run.
    fn stream_specs(
        &self,
        start: usize,
        end: usize,
        skip: &dyn Fn(usize) -> bool,
        collapse: bool,
        rows: &mut Vec<RowSpec>,
    ) {
        let mut i = start;

        while i < end {
            let item = &self.items[i].item;

            if skip(i) || hidden(item) || matches!(item, ChatItem::Working { .. }) {
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

                rows.push(RowSpec::RunToggle {
                    run_start,
                    hidden_count: visible.len() - 1,
                    expanded,
                });

                if expanded {
                    for &k in &visible {
                        rows.push(self.work_spec(k));
                    }
                } else if let Some(&last) = visible.last() {
                    rows.push(self.work_spec(last));
                }
            } else {
                for &k in &visible {
                    rows.push(self.work_spec(k));
                }
            }

            i = j;
        }
    }

    /// Diff freshly built specs against the list's current contents and
    /// notify it as narrowly as possible: an equal-count middle means rows
    /// changed in place (streaming growth, expansion) and only needs
    /// remeasuring, which preserves the scroll position exactly; a count
    /// change is a real splice.
    fn sync_transcript_list(&mut self, new: Vec<RowSpec>) {
        if self.row_specs == new {
            return;
        }

        let prefix = self
            .row_specs
            .iter()
            .zip(&new)
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = self.row_specs[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let old_mid = prefix..self.row_specs.len() - suffix;
        let new_mid = new.len() - suffix - prefix;

        if old_mid.len() == new_mid {
            self.transcript_list.remeasure_items(old_mid);
        } else {
            self.transcript_list.splice(old_mid, new_mid);
        }

        self.row_specs = new;
    }

    /// Build the element for one visible row. Row indices come from the list
    /// element during layout/paint, resolved through the spec snapshot taken
    /// in the current render pass.
    fn render_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(spec) = self.row_specs.get(ix).cloned() else {
            return div().into_any_element();
        };

        let row = match spec {
            RowSpec::Entry { index, .. } => self.render_entry_row(index, cx),
            RowSpec::Work { index, .. } => self.render_work_row(index, cx),
            RowSpec::FoldHeader {
                turn,
                seconds,
                folded,
            } => self.render_fold_header(turn, seconds, folded, cx),
            RowSpec::RunToggle {
                run_start,
                hidden_count,
                expanded,
            } => self.render_run_toggle(run_start, hidden_count, expanded, cx),
            RowSpec::Working => self.render_working_row(cx),
        };

        // The pre-virtualization container spaced rows with `gap_2` inside a
        // `p_3` body; each row now carries its own horizontal inset and
        // bottom gap.
        div().w_full().px_3().pb_2().child(row).into_any_element()
    }

    fn render_working_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(started) = self.working_started else {
            return div().into_any_element();
        };

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
            .into_any_element()
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
    ///
    /// Oversized prompts (huge pastes) collapse to their head by default:
    /// a visible row re-lays-out its full text every frame, so an unbounded
    /// prompt would make every frame O(paste size). Expansion is an explicit
    /// per-row choice, and the right-click Copy always carries the full text.
    fn render_user_row(&self, index: usize, text: String, cx: &mut Context<Self>) -> AnyElement {
        let head_len = truncated_user_prompt(&text).map(str::len);
        let expanded = head_len.is_some() && self.expanded_rows.contains(&index);
        let shown = match (head_len, expanded) {
            (Some(len), false) => text[..len].to_string(),
            _ => text,
        };
        let toggle = head_len.is_some().then(|| {
            div()
                .mt_1()
                .text_xs()
                .text_color(cx.theme().primary)
                .cursor_pointer()
                .child(if expanded {
                    "Show less".to_string()
                } else {
                    "Show full message".to_string()
                })
                .id(("user-expand", index))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.expanded_rows.insert(index) {
                        this.expanded_rows.remove(&index);
                    }
                    cx.notify();
                }))
        });

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
                    // Plain, not markdown: the prompt is user-authored text and
                    // must render verbatim, but stays drag-selectable.
                    .child(text::TextView::plain(("user-text", index), shown).selectable(true))
                    .children(toggle),
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
                summary,
                diff,
                status,
                ..
            } => (
                IconName::File,
                "Edit".to_string(),
                summary.clone(),
                Some(status.clone()),
                diff.clone().filter(|diff| !diff.trim().is_empty()),
            ),
            ChatItem::Tool {
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
                kind.clone(),
                title.clone(),
                Some(status.clone()),
                output.clone().filter(|output| !output.trim().is_empty()),
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
                // Fencing happens only for the expanded row, so collapsed
                // transcripts never pay for it. Command and tool output
                // become highlighted code blocks (sniffed language), edit
                // diffs are known-format, and reasoning stays the agent's
                // own markdown.
                let markdown = match &self.items[index].item {
                    ChatItem::Command { .. } => fenced_code_block(&detail),
                    ChatItem::FileChange { .. } => fenced_code_block_as(&detail, "diff"),
                    ChatItem::Tool { kind, title, .. } => match kind.as_str() {
                        // Markdown-native payloads: plan approvals, todo
                        // checklists, and subagent reports read as prose.
                        "TodoWrite" | "ExitPlanMode" | "Task" => detail,
                        // File reads highlight as their source language; the
                        // extension is a registry alias, unknown ones fall
                        // back to plain.
                        "Read" => {
                            let lang = file_extension_lang(title);
                            match strip_read_gutter(&detail) {
                                Some(body) => fenced_code_block_as(&body, &lang),
                                None => fenced_code_block_as(&detail, &lang),
                            }
                        }
                        _ => fenced_code_block(&detail),
                    },
                    _ => detail,
                };

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
                            .child(
                                text::TextView::markdown(("wl-md", index), markdown)
                                    .selectable(true),
                            ),
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
        let body_height =
            px((Self::HISTORY_ROW_HEIGHT * rows as f32).min(Self::HISTORY_MAX_HEIGHT));

        let body: AnyElement = if self.history_pending.is_some() {
            // Both loading and loaded bodies use the same explicit viewport
            // height. The virtual list's inferred first-frame measurement
            // must not move the composer when it replaces these placeholders.
            v_flex()
                .w_full()
                .h(body_height)
                .flex_none()
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
                .h(body_height)
                .flex_none()
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
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(px(16.))
                        .child(Scrollbar::vertical(&self.history_scroll)),
                )
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

    fn render_composer_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let branch = self.git_branch.clone().unwrap_or_else(|| {
            if self.git_branch_ready {
                "No Git branch".to_string()
            } else {
                "Detecting branch…".to_string()
            }
        });
        let branch_opacity = if self.git_branch.is_some() {
            0.72
        } else {
            0.48
        };

        let usage = self.context_window_usage.map(|usage| {
            let (label, color) = match usage.max_tokens {
                Some(max_tokens) if max_tokens > 0 => {
                    let remaining_tokens = max_tokens.saturating_sub(usage.used_tokens);
                    let remaining_percent =
                        (remaining_tokens as f64 * 100.0 / max_tokens as f64).round() as u64;
                    let color = if remaining_percent <= 10 {
                        cx.theme().danger
                    } else {
                        cx.theme().muted_foreground.opacity(0.72)
                    };

                    (
                        format!(
                            "Context {remaining_percent}% left · {}",
                            compact_token_count(remaining_tokens)
                        ),
                        color,
                    )
                }
                _ => (
                    format!("Context {} used", compact_token_count(usage.used_tokens)),
                    cx.theme().muted_foreground.opacity(0.72),
                ),
            };

            h_flex()
                .flex_none()
                .gap_1p5()
                .items_center()
                .text_color(color)
                .child(Icon::new(IconName::Gauge).size_3())
                .child(div().child(label))
        });

        v_flex()
            .w(relative(0.95))
            .rounded_b(px(12.))
            .border_1()
            .border_t_0()
            .border_color(cx.theme().border.opacity(0.6))
            .bg(cx.theme().popover)
            .pt(px(18.))
            .child(
                h_flex()
                    .w_full()
                    .min_h(px(26.))
                    .px_4()
                    .pt_1()
                    .pb_2()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .text_xs()
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_1p5()
                            .items_center()
                            .text_color(cx.theme().muted_foreground.opacity(branch_opacity))
                            .child(Icon::new(IconName::GitBranch).size_3())
                            .child(div().min_w_0().truncate().child(branch)),
                    )
                    .children(usage),
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
                |this, value, cx| {
                    this.settings.model = Some(value);
                    this.remember_thread_defaults(cx);
                },
            ))
            .child(Self::setting_picker(
                cx,
                "agent-permission",
                "permissions",
                permission_icon(self.settings.approval.as_deref()),
                self.settings.approval.clone(),
                permission_options,
                |this, value, cx| {
                    this.settings.approval = Some(value);
                    this.remember_thread_defaults(cx);
                },
            ));

        if !effort_options.is_empty() {
            row = row.child(Self::setting_picker(
                cx,
                "agent-effort",
                "effort",
                IconName::Gauge,
                // The protocol never reports the session's current effort;
                // until the user picks one, the honest label is the CLI's
                // own per-model default rather than an empty dash.
                self.settings
                    .effort
                    .clone()
                    .or_else(|| Some("default".to_string())),
                effort_options,
                |this, value, cx| {
                    this.settings.effort = Some(value.clone());
                    this.remember_thread_defaults(cx);
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
                |this, value, cx| {
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
                    this.remember_thread_defaults(cx);
                },
            ))
            .child(Self::setting_picker(
                cx,
                "agent-approval",
                "approval",
                permission_icon(self.settings.approval.as_deref()),
                self.settings.approval.clone(),
                approval_options,
                |this, value, cx| {
                    this.settings.approval = Some(value);
                    this.remember_thread_defaults(cx);
                },
            ))
            .child(Self::setting_picker(
                cx,
                "agent-sandbox",
                "sandbox",
                IconName::Shield,
                self.settings.sandbox.clone(),
                sandbox_options,
                |this, value, cx| {
                    this.settings.sandbox = Some(value);
                    this.remember_thread_defaults(cx);
                },
            ))
            .child(Self::setting_picker(
                cx,
                "agent-effort",
                "effort",
                IconName::Gauge,
                self.settings.effort.clone(),
                effort_options,
                |this, value, cx| {
                    this.settings.effort = Some(value);
                    this.remember_thread_defaults(cx);
                },
            ))
            .child(Self::setting_picker(
                cx,
                "agent-tier",
                "tier",
                IconName::Zap,
                Some(self.settings.tier.clone().unwrap_or_default()),
                tier_options,
                |this, value, cx| {
                    this.settings.tier = (!value.is_empty()).then_some(value);
                    this.remember_thread_defaults(cx);
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

#[cfg(test)]
mod prompt_truncation_tests {
    use super::truncated_user_prompt;

    #[test]
    fn short_prompts_pass_through_and_long_ones_cut_at_boundaries() {
        assert_eq!(truncated_user_prompt("hello\nworld"), None);

        let four_lines = "line\n".repeat(4);
        let head = truncated_user_prompt(&four_lines).expect("over the line cap");
        assert_eq!(head.lines().count(), 3);
        assert!(head.ends_with('\n'));

        let giant_line = "\u{4f60}".repeat(3000); // 9000 bytes, one line
        let head = truncated_user_prompt(&giant_line).expect("over the byte cap");
        assert!(head.len() <= 4096 + 3);
        assert!(giant_line.is_char_boundary(head.len()));
    }
}

#[cfg(test)]
mod read_gutter_tests {
    use super::{file_extension_lang, strip_read_gutter};

    #[test]
    fn gutter_strips_only_when_every_line_matches() {
        assert_eq!(
            strip_read_gutter("     1\u{2192}fn main() {\n     2\u{2192}}").as_deref(),
            Some("fn main() {\n}\n")
        );
        assert_eq!(strip_read_gutter("plain output"), None);
        assert_eq!(strip_read_gutter("     1\u{2192}ok\nno gutter"), None);
    }

    #[test]
    fn extension_is_the_language_tag() {
        assert_eq!(file_extension_lang("C:\\src\\main.RS"), "rs");
        assert_eq!(file_extension_lang("noext"), "");
    }
}

#[cfg(test)]
mod fence_tests {
    use super::{detect_output_language, fenced_code_block};

    #[test]
    fn fence_outgrows_backtick_runs_and_sniffs_language() {
        assert_eq!(fenced_code_block("plain output"), "```\nplain output\n```");
        assert_eq!(
            fenced_code_block("{\"key\": 1}"),
            "```json\n{\"key\": 1}\n```"
        );
        assert_eq!(detect_output_language("diff --git a/x b/x"), "diff");

        let tricky = "text with ```` four backticks";
        let fenced = fenced_code_block(tricky);
        assert!(
            fenced.starts_with("`````\n"),
            "fence must outgrow body runs"
        );
        assert!(fenced.ends_with("\n`````"));
    }
}
