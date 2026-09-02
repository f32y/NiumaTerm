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
mod session;
pub mod settings;
mod thread_controls;
pub mod transcript;
mod view;
mod workflows;

use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use futures::channel::oneshot;
use gpui::{Entity, FocusHandle, Pixels, Point, ScrollHandle, SharedString};
use gpui_component::VirtualListScrollHandle;
use gpui_component::input::InputState;
use nmt_agent_utils::chat::{
    ContextComposition, ContextWindowUsage, GoalStatus, Question, ReplayTurn, SessionScope,
    SessionStats, SessionSummary, SkillCatalog, SkillReference, SlashCommandInfo,
};
use nmt_agent_utils::{AgentEvent, AgentRoute, AgentWorkspace};
use nmt_config::profile::AgentProfile;
use nmt_i18n::i18n;

use crate::composer::attachments::PendingAttachments;
use crate::composer::{CommandFeedback, ForkFlow, PendingSlashCommand, RewindState};
use crate::input_history::{InputHistoryNavigation, InputHistoryScope};
use crate::pane_state::{ChildAgents, SessionRuntime, TurnState};
pub use crate::profile::{AgentKind, AgentThreadDefaults, agent_launch};
pub use crate::session::{
    RecoveryIdentity, RecoveryReadiness, RecoverySnapshot, RestorationReadiness,
};
use crate::thread_controls::ThreadControls;
use crate::transcript::TranscriptView;
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
    response_annotations: Vec<String>,
    skill: Option<SkillReference>,
}

/// A pending `AskUserQuestion` card and the picks made so far. Selections are
/// held as option indices per question so the rendered state and the labels
/// sent back cannot drift apart.
struct QuestionPrompt {
    questions: Vec<Question>,
    selected: Vec<Vec<usize>>,
    /// The option the arrow keys are on, as `(question, option)`. The composer
    /// input keeps the real focus while the card is up, so this is a highlight
    /// the pane draws rather than a focused widget, the same way the command
    /// palette tracks its own row.
    focus: (usize, usize),
}

impl QuestionPrompt {
    fn new(questions: Vec<Question>) -> Self {
        let selected = vec![Vec::new(); questions.len()];

        Self {
            questions,
            selected,
            focus: (0, 0),
        }
    }

    fn is_selected(&self, question: usize, option: usize) -> bool {
        self.selected[question].contains(&option)
    }

    fn is_focused(&self, question: usize, option: usize) -> bool {
        self.focus == (question, option)
    }

    /// Every option of every question, in the order they are drawn. Moving
    /// across question boundaries rather than stopping at them is what lets one
    /// pair of keys answer a whole card.
    fn options_in_order(&self) -> Vec<(usize, usize)> {
        self.questions
            .iter()
            .enumerate()
            .flat_map(|(question, entry)| {
                (0..entry.options.len()).map(move |option| (question, option))
            })
            .collect()
    }

    /// Move the highlight by one, wrapping at both ends. A card holds at most
    /// four questions of four options, so wrapping is quicker than reversing
    /// direction and cannot hide an option off-screen.
    fn move_focus(&mut self, forward: bool) -> bool {
        let order = self.options_in_order();
        if order.is_empty() {
            return false;
        }

        // A highlight that names no drawn option lands on the first one instead
        // of stepping past it, which is what happens when the leading question
        // carries no options at all.
        let Some(current) = order.iter().position(|entry| *entry == self.focus) else {
            self.focus = order[0];
            return true;
        };

        let next = if forward {
            (current + 1) % order.len()
        } else {
            (current + order.len() - 1) % order.len()
        };

        self.focus = order[next];
        true
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

    use crate::QuestionPrompt;

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

    #[test]
    fn the_highlight_walks_every_option_across_questions_and_wraps() {
        let mut prompt = QuestionPrompt::new(vec![
            question("Which database?", false, &["Postgres", "SQLite"]),
            question("Which extras?", true, &["Metrics", "Tracing"]),
        ]);

        assert!(prompt.is_focused(0, 0));

        // Down crosses the question boundary rather than stopping at it, so one
        // pair of keys reaches every option on the card.
        let walked: Vec<(usize, usize)> = (0..4)
            .map(|_| {
                prompt.move_focus(true);
                prompt.focus
            })
            .collect();
        assert_eq!(walked, vec![(0, 1), (1, 0), (1, 1), (0, 0)]);

        // Up from the first option wraps to the last.
        prompt.move_focus(false);
        assert_eq!(prompt.focus, (1, 1));
    }

    #[test]
    fn a_question_with_no_options_cannot_trap_the_highlight() {
        // The provider caps options at four but does not promise a minimum, and
        // a card that swallows the arrow keys would leave the user no way to
        // reach the options that do exist.
        let mut prompt = QuestionPrompt::new(vec![
            question("Nothing to pick", false, &[]),
            question("Which database?", false, &["Postgres", "SQLite"]),
        ]);

        // The first press reaches the first drawn option rather than stepping
        // over it, which is what an out-of-range starting highlight would do.
        assert!(prompt.move_focus(true));
        assert_eq!(prompt.focus, (1, 0));

        // A card with nothing to pick consumes no keys, so they still reach
        // whatever else is listening.
        let empty = &mut QuestionPrompt::new(vec![question("Nothing at all", false, &[])]);
        assert!(!empty.move_focus(true));
    }
}

#[cfg(test)]
mod git_branch_poll_tests {
    use crate::GitBranchPoll;

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
    /// their `[Image #N]` placeholders.
    attachments: PendingAttachments,
    /// Earlier agent response text attached to the pending message.
    response_annotations: Vec<String>,
    /// When the agent last finished answering, for the composer's idle
    /// reading of how long the conversation has been waiting on the user.
    /// `None` until the first turn settles.
    last_response_at: Option<Instant>,
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
    /// Description of the approval request blocking the turn, shown as the
    /// card above the input; the request id lives in the session.
    pending_approval: Option<String>,
    /// Questions the model wants answered before it continues, plus the
    /// selection the user has built so far. The request id lives in the
    /// session, so this holds only what the card renders.
    pending_questions: Option<QuestionPrompt>,
    palette: SlashPalette,
    rewind: RewindFlow,
    fork: ForkFlow,
    git_branch_poll: GitBranchPoll,
    context_window_usage: Option<ContextWindowUsage>,
    /// How that window is currently filled, when the provider measures it.
    /// Codex reports only accounting, so this stays empty there.
    context_composition: Option<ContextComposition>,
    /// The standing objective the backend is working towards, when it runs
    /// one. It outlives the turn that created it, so it belongs beside the
    /// composer rather than in the transcript.
    goal: Option<GoalStatus>,
    /// Whether the backend is collaborating on a plan rather than carrying out
    /// work. Backends that have no such mode never set it.
    plan_mode: bool,
    /// Whole-log counters the backend folds from its own log. Absent where the
    /// backend reports none, because counting the visible transcript instead
    /// would disagree with the conversation's real length.
    session_stats: Option<SessionStats>,
    /// Workflow runs of this session and the agent conversation the user has
    /// open. Workflow agents are not child agents, so they never reach the
    /// `Background Tasks` state above.
    workflows: WorkflowUi,
}
