pub(crate) mod input_history;
pub(crate) mod updates;
pub(super) mod usage;

mod commands;
mod composer;
mod context_usage;
mod links;
mod profile;
mod session;
pub(crate) mod transcript;
mod view;
mod workflows;

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::env;
use std::mem::take;
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Local};
use futures::StreamExt as _;
use futures::channel::{mpsc, oneshot};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClipboardItem, Context, Div, ElementId, Entity, FocusHandle, FollowMode,
    FontWeight, Hsla, ListAlignment, ListHorizontalSizingBehavior, ListSizingBehavior, ListState,
    MouseButton, Pixels, ScrollHandle, ScrollStrategy, SharedString, Stateful, StyleRefinement,
    Task, UniformListScrollHandle, Window, div, linear_color_stop, linear_gradient, list, px,
    relative, rems, size, uniform_list,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{
    Enter, Escape, IndentInline, Input, InputEvent, InputState, MoveDown, MoveUp,
};
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::radio::RadioGroup;
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
use gpui_component::skeleton::Skeleton;
use gpui_component::spinner::Spinner;
use gpui_component::text::TextViewStyle;
use gpui_component::{
    ActiveTheme as _, Disableable as _, ElementExt as _, Icon, IconName, IconNamed, Sizable as _,
    VirtualListScrollHandle, h_flex, text, v_flex, v_virtual_list,
};
use nmt_agent_utils::background_task::{
    BackgroundTaskKey, BackgroundTaskProvider, BackgroundTaskSnapshot, BackgroundTaskTranscript,
};
use nmt_agent_utils::chat::{
    Compaction, CompactionTrigger, ContextComposition, ContextWindowUsage, Event as SessionEvent,
    Item as SessionItem, ModelInfo, Question, QuestionOption, ReplayTurn, SendOutcome,
    SessionSummary, SkillCatalog, SkillInfo, SkillReference, SlashCommandArguments,
    SlashCommandInfo, SlashCommandOutcome, SlashCommandRunPolicy, SlashCommandSource,
    ThreadSettings,
};
use nmt_agent_utils::claude_code::workflows::{
    RestoredWorkflowRun, WorkflowRefreshRequest, WorkflowRefreshResult,
};
use nmt_agent_utils::claude_code::{sessions, stream_json};
use nmt_agent_utils::codex::app_server;
use nmt_agent_utils::launcher::AgentCli;
use nmt_agent_utils::update::{InstallationKey, ProviderKind};
use nmt_agent_utils::workflow::{WorkflowAgentState, WorkflowRun, WorkflowSnapshot};
use nmt_agent_utils::{
    AgentEvent, AgentEventKind, AgentRoute, CodexProviderConfig, LaunchConfig, agent_process,
    normalize_body, normalize_title,
};
use nmt_config::local_state::{self, AgentDefaults as StoredAgentDefaults};
use nmt_config::profile::{AgentProfile, AgentProfileKind};
use nmt_config::system::NewlineShortcut;
use nmt_i18n::i18n;
use serde_json::Value;
use tracing::{info, warn};

use crate::agent_pane::commands::{
    PaletteCatalogEntry, PaletteDirection, claim_command_turn_start, filter_palette_catalog,
    filter_skill_catalog, is_current_session_epoch, local_commands, merge_catalog,
    move_palette_selection, next_session_epoch, parse_skill_prefix, parse_slash_command,
    prepare_skill_selection, reconcile_skill_binding, reset_command_runtime, resolve_choice,
    setting_value_label, validate_skill_binding,
};
use crate::agent_pane::composer::{CommandFeedback, PendingSlashCommand, RewindState};
use crate::agent_pane::input_history::{InputHistoryNavigation, InputHistoryScope};
pub(crate) use crate::agent_pane::profile::{AgentKind, AgentThreadDefaults, agent_launch};
use crate::agent_pane::session::{Backend, Status, UpdateSuspension};
pub(crate) use crate::agent_pane::session::{
    RecoveryIdentity, RecoveryReadiness, RecoverySnapshot, RestorationReadiness,
};
use crate::agent_pane::transcript::{Entry, RowSpec, TranscriptView, VirtualTranscriptState};
use crate::agent_pane::workflows::WorkflowUi;
use crate::ui::{AppSettings, UI_RADIUS, WorkingIndicator, current_branch};

#[derive(Clone)]
pub(crate) enum AgentPaneEvent {
    Lifecycle(AgentEvent),
    Interrupted,
    /// This tab's workflow picture changed: it gained its first run, or its
    /// count of running agents moved. Reported as an event so the chrome can
    /// track it without observing every pane repaint.
    WorkflowActivity,
    /// This tab's count of running child agents moved. Reported as an event so
    /// the chrome can track it without observing every pane repaint.
    BackgroundTaskActivity,
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

    /// An outside click dismisses only an explicit `/resume` list. The
    /// automatic list on a blank tab is that tab's default surface, so a
    /// click on the empty pane keeps it open; hiding it there would strand
    /// the tab with no way back except `/resume`.
    fn dismisses_on_outside_click(self) -> bool {
        !matches!(self, Self::Automatic)
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

struct UnansweredPrompt {
    turn: u64,
    text: String,
    skill: Option<SkillReference>,
}

/// A pending `AskUserQuestion` card and the picks made so far. Selections are
/// held as option indices per question so the rendered state and the labels
/// sent back cannot drift apart.
struct QuestionPrompt {
    questions: Vec<Question>,
    selected: Vec<Vec<usize>>,
}

impl QuestionPrompt {
    fn new(questions: Vec<Question>) -> Self {
        let selected = vec![Vec::new(); questions.len()];

        Self {
            questions,
            selected,
        }
    }

    fn is_selected(&self, question: usize, option: usize) -> bool {
        self.selected[question].contains(&option)
    }

    /// Single-select replaces the pick; multi-select toggles it. Multi-select
    /// keeps ascending order so the answer array matches the visible order
    /// rather than the order the user happened to click in.
    fn toggle(&mut self, question: usize, option: usize) {
        let multi_select = self.questions[question].multi_select;
        let picks = &mut self.selected[question];

        if !multi_select {
            *picks = vec![option];
            return;
        }

        match picks.iter().position(|picked| *picked == option) {
            Some(index) => {
                picks.remove(index);
            }
            None => {
                picks.push(option);
                picks.sort_unstable();
            }
        }
    }

    /// Every question needs an answer: the provider reports an unanswered one
    /// as "(no option selected)", which reads to the model as a refusal.
    fn is_complete(&self) -> bool {
        self.selected.iter().all(|picks| !picks.is_empty())
    }

    fn answers(&self) -> Vec<Vec<String>> {
        self.questions
            .iter()
            .zip(&self.selected)
            .map(|(question, picks)| {
                picks
                    .iter()
                    .filter_map(|index| question.options.get(*index))
                    .map(|option| option.label.clone())
                    .collect()
            })
            .collect()
    }
}

impl GitBranchPoll {
    fn begin_refresh(&mut self) -> bool {
        if self.refreshing {
            return false;
        }
        self.refreshing = true;
        true
    }

    fn complete(&mut self, branch: Option<String>) {
        self.branch = branch;
        self.ready = true;
        self.refreshing = false;
    }

    fn presentation(&self) -> (String, f32) {
        let label = self.branch.clone().unwrap_or_else(|| {
            if self.ready {
                i18n("agent-git-no-branch").to_string()
            } else {
                i18n("agent-git-detecting-branch").to_string()
            }
        });
        let opacity = if self.branch.is_some() { 0.72 } else { 0.48 };
        (label, opacity)
    }
}

#[cfg(test)]
mod question_prompt_tests {
    use nmt_agent_utils::chat::{Question, QuestionOption};

    use super::QuestionPrompt;

    fn question(text: &str, multi_select: bool, labels: &[&str]) -> Question {
        Question {
            header: None,
            question: text.to_owned(),
            multi_select,
            options: labels
                .iter()
                .map(|label| QuestionOption {
                    label: (*label).to_owned(),
                    description: None,
                })
                .collect(),
        }
    }

    #[test]
    fn single_select_replaces_and_multi_select_toggles_in_option_order() {
        let mut prompt = QuestionPrompt::new(vec![
            question("Which database?", false, &["Postgres", "SQLite"]),
            question("Which extras?", true, &["Metrics", "Tracing", "Audit log"]),
        ]);

        assert!(!prompt.is_complete());

        prompt.toggle(0, 1);
        prompt.toggle(0, 0);
        assert!(prompt.is_selected(0, 0));
        assert!(!prompt.is_selected(0, 1));

        // Picked out of order; the answer still follows the visible order.
        prompt.toggle(1, 2);
        prompt.toggle(1, 0);
        assert!(prompt.is_complete());
        assert_eq!(
            prompt.answers(),
            vec![
                vec!["Postgres".to_owned()],
                vec!["Metrics".to_owned(), "Audit log".to_owned()],
            ]
        );

        // Re-clicking a multi-select option clears it, and clearing the last
        // pick of a question blocks submission again.
        prompt.toggle(1, 0);
        prompt.toggle(1, 2);
        assert!(!prompt.is_complete());
        assert_eq!(prompt.answers()[1], Vec::<String>::new());
    }
}

#[cfg(test)]
mod git_branch_poll_tests {
    use super::GitBranchPoll;

    #[test]
    fn refresh_state_coalesces_requests_and_updates_presentation() {
        let mut poll = GitBranchPoll::default();
        assert_eq!(poll.presentation(), ("Detecting branch…".into(), 0.48));

        assert!(poll.begin_refresh());
        assert!(!poll.begin_refresh());

        poll.complete(Some("main".into()));
        assert_eq!(poll.presentation(), ("main".into(), 0.72));

        assert!(poll.begin_refresh());
        poll.complete(None);
        assert_eq!(poll.presentation(), ("No Git branch".into(), 0.48));
    }
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
    pending_resume_replay: Option<Vec<ReplayTurn>>,
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
    /// Order of the latest feedback shown. A delayed dismissal compares
    /// against it so it can only retire the message it was started for.
    feedback_seq: u64,
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
    input_history_scope: InputHistoryScope,
    input_history_navigation: InputHistoryNavigation,
    /// The conversation as the user reads it. Presentation lives in its own
    /// view so a child agent's conversation renders through the same code.
    transcript: Entity<TranscriptView>,
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
    /// Questions the model wants answered before it continues, plus the
    /// selection the user has built so far. The request id lives in the
    /// session, so this holds only what the card renders.
    pending_questions: Option<QuestionPrompt>,
    /// Current thread settings, seeded from the session's `Ready` event and
    /// changed via the dropdowns under the input; sent as overrides on every
    /// turn start (idempotent when unchanged).
    settings: ThreadSettings,
    /// Whether the next `Ready` should overlay all remembered settings. True
    /// for fresh conversations and resumed Claude conversations; later Claude
    /// confirmations keep the values currently selected under the input.
    seed_thread_defaults: bool,
    /// Whether the next resumed Codex thread should take the locally remembered
    /// approval reviewer while preserving its other stored settings.
    seed_approval_reviewer: bool,
    /// A rewind starts a new backend identity but keeps the user's current
    /// thread controls. The first Ready payload describes process defaults,
    /// so these values are overlaid once instead of being replaced by them.
    restore_thread_settings_on_ready: Option<ThreadSettings>,
    /// Model catalog; service tiers are per model, so the tier dropdown lists
    /// the selected model's tiers.
    models: Vec<ModelInfo>,
    /// Monotonic turn counter; entries are tagged with the turn they arrived
    /// in so a settled turn can fold as one unit.
    turn_seq: u64,
    /// The active prompt remains recoverable until provider activity becomes
    /// visible, allowing an immediate stop to return it to the composer.
    unanswered_prompt: Option<UnansweredPrompt>,
    /// Turn the user asked to stop. Interruption is a completion state of a
    /// turn, so the "Interrupted" transcript row is drawn only when that turn
    /// actually ends; a backend that keeps streaming past the stop request
    /// keeps its truthful working row until then.
    pending_interrupt: Option<u64>,
    palette: SlashPalette,
    /// Mid-turn inputs stay near the composer until provider activity confirms
    /// they have joined the running response.
    queued_user_messages: VecDeque<String>,
    rewind: RewindFlow,
    git_branch_poll: GitBranchPoll,
    context_window_usage: Option<ContextWindowUsage>,
    /// How that window is currently filled, when the provider measures it.
    /// Codex reports only accounting, so this stays empty there.
    context_composition: Option<ContextComposition>,
    /// Process replacement for a provider update is pane state rather than a
    /// terminal exit. Keeping it separate retains transcript and composer
    /// contents while preventing input from reaching a missing backend.
    update_suspension: Option<UpdateSuspension>,
    last_recovery_snapshot: Option<RecoverySnapshot>,
    /// Claude session id whose child agents were already restored from
    /// history. Ready fires again during first-turn initialization, so the
    /// read happens once per conversation rather than once per confirmation.
    restored_task_session: Option<String>,
    /// Latest child-agent snapshot published by the provider adapter. The
    /// adapter owns child lifecycle; the pane keeps only this replacement copy
    /// so the right-side view never maintains a second mutable registry.
    background_tasks: Option<BackgroundTaskSnapshot>,
    /// Each child's own conversation, accumulated here rather than in the
    /// adapter so live activity is retained once and the retention bound
    /// applies to what is actually shown.
    background_task_transcripts: HashMap<BackgroundTaskKey, BackgroundTaskTranscript>,
    /// Workflow runs of this session and the agent conversation the user has
    /// open. Workflow agents are not child agents, so they never reach the
    /// `Background Tasks` state above.
    workflows: WorkflowUi,
}
