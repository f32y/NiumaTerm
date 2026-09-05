//! The agent conversation pane: one backend session per tab, the composer,
//! the transcript, thread controls, and child-agent views. Harnesses may share
//! a process while keeping their session state isolated.
//!
//! The application shell owns tabs, chrome, provider updates, and settings;
//! this crate reads only the [`settings::AgentSettings`] snapshot the shell
//! installs and exposes the pane plus the recovery types the update
//! coordinator drives across a backend replacement.

pub mod input_history;

mod capabilities;
mod commands;
mod composer;
mod context_usage;
mod links;
mod pane_state;
pub mod profile;
mod questions;
mod session;
pub mod settings;
mod thread_controls;
pub mod transcript;
mod view;
mod workflows;

use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{Entity, FocusHandle, Pixels, Point, ScrollHandle, SharedString};
use gpui_component::VirtualListScrollHandle;
use gpui_component::input::InputState;
use nmt_agent_utils::chat::{
    ContextComposition, ContextWindowUsage, ReplayTurn, SessionScope, SessionStats, SessionSummary,
    SkillCatalog, SkillReference, SlashCommandInfo,
};
use nmt_agent_utils::{AgentEvent, AgentRoute, AgentWorkspace};
use nmt_config::profile::AgentProfile;
use nmt_i18n::i18n;

use crate::composer::attachments::ComposerAttachments;
use crate::composer::{BranchFlow, CommandFeedback, PendingSlashCommand};
use crate::input_history::{InputHistoryNavigation, InputHistoryScope};
use crate::pane_state::{ChildAgents, SessionRuntime, TurnState};
pub use crate::profile::{AgentKind, AgentThreadDefaults, agent_launch};
use crate::session::prompts::PendingPrompts;
pub use crate::session::{
    RecoveryIdentity, RecoveryReadiness, RecoverySnapshot, RestorationReadiness,
};
use crate::thread_controls::ThreadControls;
use crate::transcript::TranscriptView;
use crate::view::session_state::SessionStateBadge;
use crate::workflows::WorkflowUi;

#[derive(Clone)]
pub enum AgentPaneEvent {
    Lifecycle(AgentEvent),
    Interrupted,
    /// This tab's workflow picture changed: it gained its first run, or its
    /// count of running agents moved. Reported as an event so the chrome can
    /// track it without observing every pane repaint.
    WorkflowActivity,
    /// This tab's count of running child agents moved. Reported as an event so
    /// the chrome can track it without observing every pane repaint.
    BackgroundTaskActivity,
    /// A conversation this pane listed but cannot continue: it ran in another
    /// directory, and a tab is rooted in the one it was opened for. The chrome
    /// owns tabs, so opening it where it worked is left to the chrome.
    ResumeElsewhere {
        cwd: String,
        session_id: String,
    },
    /// A name for the conversation this pane is holding, derived from the
    /// message that opened it. The pane does not know which tab owns it, so
    /// naming the tab is left to the chrome that does. An empty name means the
    /// pane no longer holds a conversation worth naming, which drops the tab
    /// back to the name its profile gives it.
    TitleSuggested(String),
    /// The tab holding this pane should close. A pane owns no tab, so the
    /// chrome that does is asked to close it.
    CloseRequested,
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
    response_annotations: Vec<String>,
    skill: Option<SkillReference>,
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
mod tests;

/// Eased position along a transition, for a parameter already clamped to
/// `0..=1`. The ramp leaves and arrives at zero speed, so neither end of a
/// transition built on it reads as the effect being switched on.
fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Ramp driving the transcript blur behind the recent-session list: where it
/// started, what it is heading for, and when it left. Reversing mid-ramp starts
/// a fresh one from wherever the previous had reached, so a list dismissed
/// while it is still opening unblurs from the blur actually on screen instead
/// of snapping to full.
#[derive(Clone, Copy)]
struct BlurFade {
    from: f32,
    to: f32,
    start: Instant,
}

impl BlurFade {
    const DURATION: Duration = Duration::from_millis(150);

    fn progress(&self, now: Instant) -> f32 {
        let elapsed = now.duration_since(self.start).as_secs_f32();
        let t = (elapsed / Self::DURATION.as_secs_f32()).clamp(0.0, 1.0);

        self.from + (self.to - self.from) * smoothstep(t)
    }

    fn settled(&self, now: Instant) -> bool {
        now.duration_since(self.start) >= Self::DURATION
    }
}

impl Default for BlurFade {
    fn default() -> Self {
        Self {
            from: 0.0,
            to: 0.0,
            start: Instant::now(),
        }
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
    /// The rows answer a search rather than list what is recent. History pages
    /// accumulate, so without this the next page would be appended to the
    /// matches and the strip would mix two different questions' answers.
    showing_search: bool,
    /// The one highlighted row, whether the pointer or the arrow keys put it
    /// there. A list has a single current row: what a click opens and what
    /// Enter opens are the same row, and only one thing on screen says so.
    selected: usize,
    /// Whether the pointer is over the list. A search narrows the rows while
    /// the arrow keys still belong to the input, so the keyboard's highlight
    /// is not drawn then; a pointer over the list is reason enough to draw it,
    /// because the row under the pointer is what a click would open.
    pointer_inside: bool,
    /// Where the pointer last was over the list, so a row sliding under a
    /// pointer that has not moved cannot take the highlight back. Keyboard
    /// navigation scrolls the list, which does exactly that.
    pointer: Option<Point<Pixels>>,
    /// Claude replay is loaded before process replacement and published only
    /// after the resumed process confirms readiness.
    pending_resume_replay: Option<Vec<ReplayTurn>>,
    /// Which directories the rows come from. A conversation belongs to the
    /// directory it ran in, so widening the list means rows this tab cannot
    /// resume in place; those open where they worked instead.
    scope: SessionScope,
    scroll: VirtualListScrollHandle,
    transcript_blur: BlurFade,
}

impl Default for SessionHistoryUi {
    fn default() -> Self {
        Self {
            sessions: Vec::new(),
            pending: None,
            mode: RecentSessionsMode::Automatic,
            showing_search: false,
            selected: 0,
            pointer_inside: false,
            pointer: None,
            pending_resume_replay: None,
            scope: SessionScope::default(),
            scroll: VirtualListScrollHandle::new(),
            transcript_blur: BlurFade::default(),
        }
    }
}

/// Translated text as a `SharedString` that borrows rather than copies. Both
/// catalogs are parsed once into maps that are never dropped, so the text stays
/// valid for the life of the process and a view rebuilt every frame pays
/// nothing per label.
pub(crate) fn translated(key: &'static str) -> SharedString {
    SharedString::new_static(i18n(key))
}

/// The merged `/` catalog, held so it is not rebuilt from the local, adapter,
/// and provider lists on every frame the palette paints. `language` is part of
/// the key because local entries carry translated descriptions and the user can
/// switch language while a pane is open.
struct CachedCatalog {
    language: u8,
    commands: Rc<[SlashCommandInfo]>,
}

/// Slash-command palette, skill picker, and pending-command state.
#[derive(Default)]
struct SlashPalette {
    /// Provider discovery is a replacement snapshot; adapter/local entries
    /// remain available independently of whether discovery has arrived.
    provider_commands: Vec<SlashCommandInfo>,
    provider_commands_ready: bool,
    /// Derived from `provider_commands`; every write to that list must drop
    /// this, or the palette keeps offering commands the harness has withdrawn.
    catalog: Option<CachedCatalog>,
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

pub struct AgentPane {
    pub(crate) focus: FocusHandle,
    agent_route: AgentRoute,
    kind: AgentKind,
    /// The launch profile this pane was opened with (executable, endpoint,
    /// env vars); every session (re)start uses it.
    profile: AgentProfile,
    /// The directories this tab is configured with. The primary one is the
    /// tab's working directory: the session process runs there and provider
    /// session history is scoped to it, because a resume id only resolves
    /// against the directory its conversation ran in. Editing the parent
    /// workspace replaces this list for the next conversation.
    workspace: AgentWorkspace,
    /// The directory list the running conversation was started with. Held
    /// apart from `workspace` so an edit never changes what a process already
    /// running was granted.
    active_workspace: AgentWorkspace,
    input_history_scope: InputHistoryScope,
    input_history_navigation: InputHistoryNavigation,
    /// Images the pending message carries, anchored to the composer text by
    /// their `[Image #N]` placeholders, and the response text quoted into it.
    attachments: ComposerAttachments,
    /// Whether this conversation has already named its tab. Only the message
    /// that opens a conversation names it: a later one is a follow-up on the
    /// same subject, and renaming on every send would make the tab strip
    /// churn under a working agent.
    conversation_named: bool,
    /// A user rename committed before the provider published its conversation
    /// identity. Ready applies it once the backend can address the thread.
    pending_conversation_rename: Option<String>,
    /// The conversation as the user reads it. Presentation lives in its own
    /// view so a child agent's conversation renders through the same code.
    transcript: Entity<TranscriptView>,
    input: Entity<InputState>,
    history_ui: SessionHistoryUi,
    /// The backend process and its lifecycle; a (re)spawn replaces it whole.
    runtime: SessionRuntime,
    /// Thread controls under the composer: values, catalogs, seeding flags.
    controls: ThreadControls,
    /// The running turn's bookkeeping, from submission to settled output.
    turn: TurnState,
    /// Child-agent activity the provider adapter reports for this session.
    children: ChildAgents,
    /// The approval and question cards that block a turn until answered.
    prompts: PendingPrompts,
    palette: SlashPalette,
    /// Cutting the conversation at an earlier point, by rewind or by fork.
    branch: BranchFlow,
    git_branch_poll: GitBranchPoll,
    context_window_usage: Option<ContextWindowUsage>,
    /// How that window is currently filled, when the provider measures it.
    /// Codex reports only accounting, so this stays empty there.
    context_composition: Option<ContextComposition>,
    /// The plan mode and standing objective drawn above the composer.
    session_state: SessionStateBadge,
    /// Whole-log counters the backend folds from its own log. Absent where the
    /// backend reports none, because counting the visible transcript instead
    /// would disagree with the conversation's real length.
    session_stats: Option<SessionStats>,
    /// Workflow runs of this session and the agent conversation the user has
    /// open. Workflow agents are not child agents, so they never reach the
    /// `Background Tasks` state above.
    workflows: WorkflowUi,
}
