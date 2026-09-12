//! The agent conversation pane: one backend session per tab, the composer,
//! the transcript, thread controls, and child-agent views. Harnesses may share
//! a process while keeping their session state isolated.
//!
//! The application shell owns tabs, chrome, provider updates, and settings;
//! this module reads only the [`settings::AgentSettings`] snapshot the shell
//! installs and exposes the pane plus the recovery types the update
//! coordinator drives across a backend replacement.

use nmt_agent::session::controller::SessionController;
use nmt_agent::session::history::SessionHistory;

pub mod input_history;

mod capabilities;
mod commands;
mod composer;
mod context_usage;
pub mod execution;
mod fade;
mod pane_state;
pub mod profile;
mod questions;
mod session;
pub mod settings;
mod thread_controls;
pub mod transcript;
mod view;
mod workflows;

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Entity, FocusHandle, Pixels, Point, ScrollHandle, SharedString, WeakEntity};
use gpui_component::VirtualListScrollHandle;
use gpui_component::input::TextareaState;
use nmt_agent::chat::{SkillCatalog, SkillReference, SlashCommandInfo};
use nmt_agent::{AgentEvent, AgentRoute, AgentWorkspace};
use nmt_config::profile::AgentProfile;
use nmt_i18n::i18n;

use crate::agent_tab::composer::attachments::ComposerAttachments;
use crate::agent_tab::composer::{BranchFlow, CommandFeedback};
#[cfg(test)]
use crate::agent_tab::execution::SessionOwner;
use crate::agent_tab::execution::{AgentSession, CommandBinding};
use crate::agent_tab::fade::Fade;
use crate::agent_tab::input_history::{InputHistoryNavigation, InputHistoryScope};
use crate::agent_tab::pane_state::TurnPresentation;
pub use crate::agent_tab::profile::{AgentKind, AgentKindExt, AgentThreadDefaults, agent_launch};
use crate::agent_tab::session::prompts::PendingPrompts;
pub use crate::agent_tab::session::{
    RecoveryIdentity, RecoveryReadiness, RecoverySnapshot, RestorationReadiness,
};
use crate::agent_tab::thread_controls::ThreadControls;
use crate::agent_tab::transcript::TranscriptView;
use crate::agent_tab::view::session_state::SessionStateBadge;
use crate::agent_tab::workflows::WorkflowUi;

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
    /// The automatic list is a blank tab's default surface, and a composer
    /// with anything in it -- typed text or the placeholder a pasted image
    /// leaves -- means the tab is being used for a new conversation, so the
    /// list steps aside and comes back once the composer is empty again. An
    /// explicit `/resume` list stays up over text: typing into it narrows
    /// the rows.
    fn is_visible(self, transcript_empty: bool, composer_empty: bool, rows: usize) -> bool {
        rows > 0
            && match self {
                Self::Automatic => transcript_empty && composer_empty,
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
    generation: u64,
}

impl GitBranchPoll {
    fn invalidate(&mut self) {
        self.generation += 1;
        self.branch = None;
        self.ready = false;
        self.refreshing = false;
    }

    fn begin_refresh(&mut self) -> Option<u64> {
        if self.refreshing {
            return None;
        }

        self.refreshing = true;

        Some(self.generation)
    }

    fn complete(&mut self, generation: u64, branch: Option<String>) {
        if generation != self.generation {
            return;
        }

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

/// Recent-session list shown above the composer.
struct SessionHistoryUi {
    data: SessionHistory,

    /// Blank conversations show the list automatically; `/resume` can reopen
    /// the same list after a conversation has started.
    mode: RecentSessionsMode,

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

    scroll: VirtualListScrollHandle,
    transcript_blur: Fade,
}

impl Default for SessionHistoryUi {
    fn default() -> Self {
        Self {
            data: SessionHistory::default(),
            mode: RecentSessionsMode::Automatic,
            selected: 0,
            pointer_inside: false,
            pointer: None,
            scroll: VirtualListScrollHandle::new(),
            transcript_blur: Fade::default(),
        }
    }
}

/// Translated text as a `SharedString` that borrows rather than copies. Both
/// catalogs are parsed once into maps that are never dropped, so the text stays
/// valid for the life of the process and a view rebuilt every frame pays
/// nothing per label.
pub(in crate::agent_tab) fn translated(key: &'static str) -> SharedString {
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
}

pub struct AgentPane {
    pub(in crate::agent_tab) focus: FocusHandle,
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

    /// The conversation as the user reads it. Presentation lives in its own
    /// view so a child agent's conversation renders through the same code.
    transcript: Entity<TranscriptView>,

    input: Entity<TextareaState>,
    history_ui: SessionHistoryUi,

    /// Provider state and transitions, independent of widgets and rendering.
    session: Rc<RefCell<SessionController>>,

    host: WeakEntity<AgentSession>,
    binding: CommandBinding,
    presenting_session_effect: bool,
    #[cfg(test)]
    owned_session: Option<SessionOwner>,

    /// Interaction state for the thread controls under the composer.
    controls: ThreadControls,

    /// The running turn's bookkeeping, from submission to settled output.
    turn: TurnPresentation,

    /// The approval and question cards that block a turn until answered.
    prompts: PendingPrompts,

    palette: SlashPalette,

    /// Cutting the conversation at an earlier point, by rewind or by fork.
    branch: BranchFlow,

    git_branch_poll: GitBranchPoll,

    /// The plan mode and standing objective drawn above the composer.
    session_state: SessionStateBadge,

    /// Workflow runs of this session and the agent conversation the user has
    /// open. Workflow agents are not child agents, so they never reach the
    /// `Background Tasks` state above.
    workflows: WorkflowUi,

    /// Ramp of the layer that covers the pane while its backend cannot take
    /// input. Cross-fading the whole layer keeps its arrival readable as the
    /// tab being held rather than as a blur being switched on.
    overlay_fade: Fade,
}
