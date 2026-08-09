pub(crate) mod updates;
pub(super) mod usage;

mod commands;
mod composer;
mod links;
mod profile;
mod session;
mod transcript;
mod view;

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::env;
use std::mem::take;
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use chrono::Local;
use futures::StreamExt as _;
use futures::channel::{mpsc, oneshot};
use gpui::prelude::*;
use gpui::{
    AnyElement, ClipboardItem, Context, Div, ElementId, Entity, FocusHandle, FollowMode,
    FontWeight, Hsla, ListAlignment, ListHorizontalSizingBehavior, ListSizingBehavior, ListState,
    MouseButton, Pixels, ScrollHandle, ScrollStrategy, SharedString, Stateful, Task,
    UniformListScrollHandle, Window, div, linear_color_stop, linear_gradient, list, px, relative,
    rems, size, uniform_list,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{
    Enter, Escape, IndentInline, Input, InputEvent, InputState, MoveDown, MoveUp,
};
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
use gpui_component::skeleton::Skeleton;
use gpui_component::spinner::Spinner;
use gpui_component::text::TextViewStyle;
use gpui_component::{
    ActiveTheme as _, Disableable as _, ElementExt as _, Icon, IconName, IconNamed, Sizable as _,
    VirtualListScrollHandle, h_flex, text, v_flex, v_virtual_list,
};
use nmt_agent_utils::chat::{
    Compaction, CompactionTrigger, ContextWindowUsage, Event as SessionEvent, Item as SessionItem,
    ModelInfo, SendOutcome, SessionSummary, SkillCatalog, SkillInfo, SkillReference,
    SlashCommandArguments, SlashCommandInfo, SlashCommandOutcome, SlashCommandRunPolicy,
    SlashCommandSource, ThreadSettings,
};
use nmt_agent_utils::claude_code::{sessions, stream_json};
use nmt_agent_utils::codex::app_server;
use nmt_agent_utils::launcher::AgentCli;
use nmt_agent_utils::update::{InstallationKey, ProviderKind};
use nmt_agent_utils::{
    AgentEvent, AgentEventKind, AgentRoute, CodexProviderConfig, LaunchConfig, agent_process,
    normalize_body, normalize_title,
};
use nmt_config::local_state::AgentDefaults as StoredAgentDefaults;
use nmt_config::profile::{AgentProfile, AgentProfileKind};
use serde_json::Value;
use tracing::{info, warn};

use crate::agent_pane::commands::{
    PaletteCatalogEntry, PaletteDirection, claim_command_turn_start, filter_palette_catalog,
    filter_skill_catalog, is_current_session_epoch, local_commands, merge_catalog,
    move_palette_selection, next_session_epoch, parse_slash_command, prepare_skill_selection,
    reconcile_skill_binding, reset_command_runtime, resolve_choice, validate_skill_binding,
};
use crate::agent_pane::composer::{CommandFeedback, PendingSlashCommand, RewindState};
pub(crate) use crate::agent_pane::profile::{AgentKind, AgentThreadDefaults, agent_launch};
use crate::agent_pane::session::{Backend, Status, UpdateSuspension};
pub(crate) use crate::agent_pane::session::{
    RecoveryIdentity, RecoveryReadiness, RecoverySnapshot, RestorationReadiness,
};
use crate::agent_pane::transcript::{Entry, RowSpec, VirtualTranscriptState};
use crate::ui::{AppSettings, UI_RADIUS, current_branch};

#[derive(Clone)]
pub(crate) enum AgentPaneEvent {
    Lifecycle(AgentEvent),
    Interrupted,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum RecentSessionsMode {
    #[default]
    Automatic,
    Hidden,
    Open,
    Loading,
}

impl RecentSessionsMode {
    fn is_visible(self, transcript_empty: bool, rows: usize) -> bool {
        rows > 0
            && match self {
                Self::Automatic => transcript_empty,
                Self::Open => true,
                Self::Hidden | Self::Loading => false,
            }
    }
}

/// Rewind is a local multi-step operation, not a model turn. Keeping its
/// state separate prevents timers, transcript rows, and slash queues from
/// treating file restoration or session forking as provider output.
#[derive(Default)]
struct RewindFlow {
    state: Option<RewindState>,
    operation_seq: u64,
    file_completion: Option<oneshot::Sender<Result<(), String>>>,
}

/// Background-refreshed git branch of the pane's working directory.
#[derive(Default)]
struct GitBranchPoll {
    branch: Option<String>,
    ready: bool,
    refreshing: bool,
}

/// Recent-session list shown above the composer.
struct SessionHistoryUi {
    /// Resumable sessions for this cwd, newest first; shown above the
    /// composer while the transcript is empty.
    sessions: Vec<SessionSummary>,
    /// Set between the cheap count pass and the title-parsing pass: the list
    /// reserves its final height with this many placeholder rows, so the
    /// composer doesn't jump when real rows land.
    pending: Option<usize>,
    /// Blank conversations show the list automatically; `/resume` can reopen
    /// the same list after a conversation has started.
    mode: RecentSessionsMode,
    selected: usize,
    /// Claude replay is loaded before process replacement and published only
    /// after the resumed process confirms readiness.
    pending_resume_replay: Option<Vec<SessionItem>>,
    scroll: VirtualListScrollHandle,
}

impl Default for SessionHistoryUi {
    fn default() -> Self {
        Self {
            sessions: Vec::new(),
            pending: None,
            mode: RecentSessionsMode::Automatic,
            selected: 0,
            pending_resume_replay: None,
            scroll: VirtualListScrollHandle::new(),
        }
    }
}

/// Slash-command palette, skill picker, and pending-command state.
#[derive(Default)]
struct SlashPalette {
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
    selected: usize,
    dismissed: bool,
    scroll: ScrollHandle,
    feedback: Option<CommandFeedback>,
    command_queue: VecDeque<PendingSlashCommand>,
    /// An accepted backend command starts the progress clock only after the
    /// protocol reports a real turn, not when the request is written.
    awaiting_command_turn: bool,
}

pub(crate) struct AgentPane {
    pub(crate) focus: FocusHandle,
    agent_route: AgentRoute,
    kind: AgentKind,
    /// The launch profile this pane was opened with (executable, endpoint,
    /// env vars); every session (re)start uses it.
    profile: AgentProfile,
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
    /// Virtual rows cache measured heights; a width change can rewrap prose
    /// without changing row fingerprints, so the viewport width is tracked too.
    transcript_width: Option<Pixels>,
    input: Entity<InputState>,
    session: Option<Backend>,
    /// Bumped on every (re)spawn; the message pump and EOF handler of an
    /// older session compare against it and stand down, so deliberately
    /// replacing the session (resume) doesn't route stale messages into the
    /// new one or report a bogus exit.
    session_epoch: u64,
    status: Status,
    history_ui: SessionHistoryUi,
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
    /// A rewind starts a new backend identity but keeps the user's current
    /// thread controls. The first Ready payload describes process defaults,
    /// so these values are overlaid once instead of being replaced by them.
    restore_thread_settings_on_ready: Option<ThreadSettings>,
    /// Model catalog; service tiers are per model, so the tier dropdown lists
    /// the selected model's tiers.
    models: Vec<ModelInfo>,
    /// Collapsed work-log runs the user has expanded, keyed by the index of
    /// the run's first transcript entry (stable — the list only appends).
    expanded_groups: HashSet<usize>,
    /// Completed turns the user has unfolded (completed turns fold their
    /// intermediate work rows behind a "Worked for Ns" header by default).
    expanded_turns: HashSet<u64>,
    /// Settled turn durations drive fold headers without masquerading as
    /// provider transcript items.
    completed_turn_seconds: HashMap<u64, u64>,
    /// Work-log rows whose detail (command output, reasoning text) is
    /// expanded, keyed by transcript index.
    expanded_rows: HashSet<usize>,
    /// Long expanded code transcripts retain their segmented source and
    /// independent uniform-list position while visible. Collapsing a row drops
    /// the duplicate source so large outputs do not stay resident twice.
    virtual_transcripts: HashMap<usize, VirtualTranscriptState>,
    /// Monotonic turn counter; entries are tagged with the turn they arrived
    /// in so a settled turn can fold as one unit.
    turn_seq: u64,
    /// Start time of the running turn. While set, a ticking
    /// "Working for Ns" row renders at the transcript end; cleared into a
    /// permanent "Worked for Ns" fold header when the turn completes.
    working_started: Option<Instant>,
    palette: SlashPalette,
    /// Mid-turn inputs stay near the composer until provider activity confirms
    /// they have joined the running response.
    queued_user_messages: VecDeque<String>,
    rewind: RewindFlow,
    git_branch_poll: GitBranchPoll,
    context_window_usage: Option<ContextWindowUsage>,
    /// The backend is compacting the conversation. Turn output pauses for the
    /// duration, so the live progress row explains the wait instead of leaving
    /// a bare seconds counter that looks stalled.
    compacting: bool,
    /// Process replacement for a provider update is pane state rather than a
    /// terminal exit. Keeping it separate retains transcript and composer
    /// contents while preventing input from reaching a missing backend.
    update_suspension: Option<UpdateSuspension>,
    last_recovery_snapshot: Option<RecoverySnapshot>,
}
