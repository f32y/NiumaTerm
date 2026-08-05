//! Backend-neutral vocabulary for agent chat sessions. Each agent backend
//! (Codex app-server, Claude Code stream-json) translates its own wire
//! protocol into these types, so the chat UI renders one transcript model
//! and never touches protocol strings.

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

/// What a chat UI needs to react to, in transcript order.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Handshake finished; carries the thread's effective settings so the UI
    /// can seed its pickers with real values.
    Ready(ThreadSettings),
    Models(Vec<ModelInfo>),
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
