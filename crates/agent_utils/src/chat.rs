//! Backend-neutral vocabulary for agent chat sessions. Each agent backend
//! (Codex app-server, Claude Code stream-json) translates its own wire
//! protocol into these types, so the chat UI renders one transcript model
//! and never touches protocol strings.

use std::time::SystemTime;

/// Thread settings a chat UI lets the user pick. Field meanings are
/// per-backend: Codex sends them as overrides on every `turn/start`;
/// Claude stores its permission mode in `approval` and applies changes via
/// control requests before the next message (`sandbox` and `tier` unused).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThreadSettings {
    pub model: Option<String>,
    pub approval: Option<String>,
    pub sandbox: Option<String>,
    pub effort: Option<String>,
    /// `None` is the normal tier: the model catalog only lists additional
    /// tiers, so normal is expressed as an explicit `serviceTier: null`
    /// (double-optional on the wire — null resets, absent keeps).
    pub tier: Option<String>,
}

/// One entry of a backend's model catalog.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelInfo {
    pub model: String,
    pub display: String,
    /// `(tier id, tier name)` of the model's additional service tiers.
    pub tiers: Vec<(String, String)>,
    pub default_tier: Option<String>,
    /// Reasoning-effort levels the model supports; empty when the model has
    /// no effort control (or the backend keeps a global effort list instead).
    pub efforts: Vec<String>,
}

/// Which layer contributed a slash command. The UI uses this only for
/// deterministic precedence when two layers advertise the same name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlashCommandSource {
    Local,
    Adapter,
    Provider,
}

/// Shape of the input accepted after a command name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlashCommandArguments {
    None,
    Freeform,
    Choices,
    Skills,
}

/// When a command may run relative to a model turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlashCommandRunPolicy {
    Immediate,
    QueueUntilIdle,
    IdleOnly,
}

/// Backend-neutral command metadata used by the composer palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlashCommandInfo {
    /// Normalized wire name without the leading slash.
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
    pub source: SlashCommandSource,
    pub arguments: SlashCommandArguments,
    pub run_policy: SlashCommandRunPolicy,
}

/// One provider-discovered skill. `path` is part of the identity because
/// Codex can publish the same skill name from multiple configuration scopes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub path: String,
    pub scope: String,
    pub enabled: bool,
    pub display_name: Option<String>,
}

/// Complete skill-directory state for the current backend session. Errors
/// can coexist with usable entries when one configured working directory or
/// skill file fails to load.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkillCatalog {
    pub skills: Vec<SkillInfo>,
    pub errors: Vec<String>,
}

/// Exact provider identity selected by the UI for a structured skill input.
/// The catalog is revalidated before this reference is sent on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillReference {
    pub name: String,
    pub path: String,
}

/// Immediate result of asking a backend to execute a slash command. Turn
/// lifecycle remains event-driven: `Accepted` does not imply `TurnStarted`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlashCommandOutcome {
    Accepted,
    Completed { message: Option<String> },
    Rejected { message: String },
    NotReady,
}

/// A typed view of one transcript item, used for both started and completed
/// notifications. `Option` fields mean "absent in this payload — keep what
/// streaming already produced".
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// Echo of our own turn input; a UI that renders the user message locally
    /// skips these.
    UserMessage,
    AgentMessage {
        id: String,
        text: Option<String>,
    },
    Reasoning {
        id: String,
        summary: Option<String>,
    },
    CommandExecution {
        id: String,
        command: String,
        aggregated_output: Option<String>,
        status: Option<String>,
        exit_code: Option<i64>,
    },
    FileChange {
        id: String,
        paths: String,
        status: Option<String>,
    },
    /// Every other tool-call kind (mcpToolCall, webSearch, dynamicToolCall,
    /// …): kind + best-effort title, so no activity is invisible.
    Other {
        id: String,
        kind: String,
        title: String,
        status: Option<String>,
    },
}

/// One resumable persisted session, for the history list an empty chat tab
/// shows above its composer. Ordered newest-first by `last_active`.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSummary {
    pub id: String,
    /// First user prompt of the session (or an id prefix when none exists).
    pub title: String,
    pub branch: Option<String>,
    pub last_active: SystemTime,
}

/// One transcript entry reconstructed from a persisted session when resuming.
/// Only conversation text is replayed; runs of tool/command activity collapse
/// into a count, so replay cost tracks dialogue length rather than tool
/// volume.
#[derive(Clone, Debug, PartialEq)]
pub enum ReplayItem {
    User { text: String },
    Agent { text: String },
    Tools { count: usize },
}

/// What a chat UI needs to react to, in transcript order.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Handshake finished; carries the thread's effective settings so the UI
    /// can seed its pickers with real values.
    Ready(ThreadSettings),
    Models(Vec<ModelInfo>),
    /// Replacement snapshot of provider-discovered slash commands.
    Commands(Vec<SlashCommandInfo>),
    /// Replacement snapshot of provider-discovered skills and load errors.
    Skills(SkillCatalog),
    /// Asynchronous provider/RPC acknowledgement for a command request.
    /// Actual model work still uses the ordinary turn lifecycle events.
    SlashCommandResult {
        name: String,
        outcome: SlashCommandOutcome,
    },
    TurnStarted,
    TurnCompleted {
        error: Option<String>,
    },
    ItemStarted(Item),
    ItemCompleted(Item),
    AgentMessageDelta {
        item_id: String,
        delta: String,
    },
    ReasoningSummaryDelta {
        item_id: String,
        delta: String,
    },
    CommandOutputDelta {
        item_id: String,
        delta: String,
    },
    /// A server→client approval request is blocking the turn; answer with
    /// the session's `respond_approval`.
    ApprovalRequested {
        description: String,
    },
    /// The pending approval was answered or cleared by turn lifecycle.
    ApprovalResolved,
    /// Resumable sessions for the tab's working directory, newest first.
    History(Vec<SessionSummary>),
    /// Reconstructed transcript of a resumed session, to pre-fill the UI.
    Replay(Vec<ReplayItem>),
    StatusDetail(Option<String>),
    Error {
        message: String,
        /// The handshake itself failed; the session will not become usable.
        fatal: bool,
    },
}

/// Outcome of a session's `send_user_message`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    /// A new turn was started.
    StartedTurn,
    /// The message was steered into the already-running turn.
    Steered,
    /// The handshake has not produced a thread yet.
    NotReady,
}
