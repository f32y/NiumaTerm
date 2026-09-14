use crate::chat::{
    ContextComposition, ContextWindowUsage, ForkCheckpoint, GoalStatus, Item, SessionStats,
    SessionSummary, SkillCatalog, SlashCommandInfo, SlashCommandOutcome, TeamDecisionRequest,
    TurnRetry,
};
use crate::session::branch::BranchUpdate;
use crate::session::controller::{SessionFailure, SessionReady, SessionReplay};
use crate::session::input::QuestionCompletion;
use crate::transcript::TextField;

/// What the host must present after the conversation has applied a provider event.
/// Payloads move through this result once; no transcript snapshot is constructed.
pub enum SessionEffect {
    Unchanged,
    Changed,
    Ready(SessionReady),
    Commands(Vec<SlashCommandInfo>),
    Skills(SkillCatalog),

    CommandResult {
        name: String,
        outcome: SlashCommandOutcome,
        advance: bool,
    },

    TurnStarted {
        opened: bool,
    },

    ProviderTurnAccepted {
        id: String,
    },

    TeamDecision(TeamDecisionRequest),

    ProviderTurnFinished {
        id: String,
        error: Option<String>,
    },

    TurnCompleted {
        error: Option<String>,
        interrupted: bool,
    },

    OutputTokens(u64),
    ContextWindow(ContextWindowUsage),
    ContextComposition(ContextComposition),
    CompactionStarted,

    CompactionFinished {
        error: Option<String>,
    },

    Branch(BranchUpdate),
    ItemStarted(Item),
    ItemCompleted(Item),

    TextDelta {
        item_id: String,
        delta: String,
        field: TextField,
    },

    ApprovalRequested,
    ApprovalResolved,

    InputRequested {
        index: usize,
    },

    InputResolved(QuestionCompletion),
    BackgroundActivity,

    Workflows {
        activity_changed: bool,
    },

    History(Vec<SessionSummary>),
    SearchResults(Vec<SessionSummary>),
    ConfirmedPrompts(Vec<String>),
    Goal(Option<GoalStatus>),
    PlanMode(bool),
    Title(String),
    Stats(SessionStats),
    Replay(SessionReplay),
    StatusDetail(Option<TurnRetry>),
    ForkCheckpoints(Result<Vec<ForkCheckpoint>, String>),

    HostExited {
        message: String,
    },

    Error {
        message: String,
        fatal: bool,
        failure: SessionFailure,
    },

    EffortRejected {
        message: String,
    },
}
