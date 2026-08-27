//! The pane's state, grouped by the concern that changes it.
//!
//! [`crate::AgentPane`] coordinates one conversation across a backend
//! process, thread controls, per-turn bookkeeping, and child-agent activity.
//! Each group below holds the fields one of those concerns mutates together,
//! so a reader can tell from the type which fields move as a unit and which
//! merely live on the same pane.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use nmt_agent_utils::background_task::{
    BackgroundTaskKey, BackgroundTaskSnapshot, BackgroundTaskTranscript,
};
use nmt_agent_utils::chat::{AgentPreset, ApprovalPreset, ModelInfo, QueuedPrompt, ThreadSettings};

use crate::UnansweredPrompt;
use crate::session::{Backend, RecoverySnapshot, Status, UpdateSuspension};

/// The backend process and its lifecycle: everything a (re)spawn replaces.
pub(crate) struct SessionRuntime {
    pub(crate) backend: Option<Backend>,
    /// Bumped on every (re)spawn; the message pump and EOF handler of an
    /// older session compare against it and stand down, so deliberately
    /// replacing the session (resume) doesn't route stale messages into the
    /// new one or report a bogus exit.
    pub(crate) epoch: u64,
    pub(crate) status: Status,
    /// Why the backend never came up, while that is still the pane's whole
    /// state. Held rather than derived from [`Status::Exited`], which a
    /// conversation that ran and then ended also reaches.
    pub(crate) start_failure: Option<String>,
    /// Whether the start has taken long enough to be worth covering the tab
    /// for. Set by a timer rather than compared against a clock at render
    /// time, because a pane waiting on its backend repaints for nothing else.
    pub(crate) start_overlay_visible: bool,
    /// Process replacement for a provider update is pane state rather than a
    /// terminal exit. Keeping it separate retains transcript and composer
    /// contents while preventing input from reaching a missing backend.
    pub(crate) update_suspension: Option<UpdateSuspension>,
    pub(crate) last_recovery_snapshot: Option<RecoverySnapshot>,
}

/// The thread controls under the composer: current values, catalogs to pick
/// from, and the seeding flags that decide what the next `Ready` overlays.
pub(crate) struct ThreadControls {
    /// Current thread settings, seeded from the session's `Ready` event and
    /// changed via the dropdowns under the input; sent as overrides on every
    /// turn start (idempotent when unchanged).
    pub(crate) settings: ThreadSettings,
    /// Whether the next `Ready` should overlay all remembered settings. True
    /// for fresh conversations and resumed Claude conversations; later Claude
    /// confirmations keep the values currently selected under the input.
    pub(crate) seed_thread_defaults: bool,
    /// Whether the next resumed Codex thread should take the locally
    /// remembered approval reviewer while preserving its other stored
    /// settings.
    pub(crate) seed_approval_reviewer: bool,
    /// A rewind starts a new backend identity but keeps the user's current
    /// thread controls. The first Ready payload describes process defaults,
    /// so these values are overlaid once instead of being replaced by them.
    pub(crate) restore_on_ready: Option<ThreadSettings>,
    /// Model catalog; service tiers are per model, so the tier dropdown lists
    /// the selected model's tiers.
    pub(crate) models: Vec<ModelInfo>,
    /// Execution-permission presets, for a harness whose preset table belongs
    /// to its deployment. Empty for one whose presets this UI can name
    /// itself.
    pub(crate) approval_presets: Vec<ApprovalPreset>,
    /// Agent compositions this deployment offers, and the one this
    /// conversation was built from. Empty where the deployment composes none,
    /// which is a picker with nothing to choose between rather than an
    /// unsupported one.
    pub(crate) agent_presets: Vec<AgentPreset>,
    pub(crate) agent_preset: Option<String>,
    /// Stop the effort slider's thumb is being dragged to while the button is
    /// down. The level itself is applied on release.
    pub(crate) effort_drag: Option<usize>,
}

/// One turn's bookkeeping, from submission to settled output.
pub(crate) struct TurnState {
    /// Monotonic turn counter; entries are tagged with the turn they arrived
    /// in so a settled turn can fold as one unit.
    pub(crate) seq: u64,
    /// When the running turn was handed to the backend, kept until its first
    /// visible output answers it. Measured from submission rather than from
    /// the backend's turn-started event, so the reading covers the whole wait
    /// the user sat through, CLI and RPC latency included.
    pub(crate) submitted_at: Option<Instant>,
    /// How long the last turn took to produce anything visible. Survives the
    /// turn so the composer keeps reporting it while the conversation is
    /// idle.
    pub(crate) first_output_latency: Option<Duration>,
    /// The active prompt remains recoverable until provider activity becomes
    /// visible, allowing an immediate stop to return it to the composer.
    pub(crate) unanswered_prompt: Option<UnansweredPrompt>,
    /// Turn the user asked to stop. Interruption is a completion state of a
    /// turn, so the "Interrupted" transcript row is drawn only when that turn
    /// actually ends; a backend that keeps streaming past the stop request
    /// keeps its truthful working row until then.
    pub(crate) pending_interrupt: Option<u64>,
    /// Mid-turn inputs stay near the composer until provider activity
    /// confirms they have joined the running response. A backend that owns
    /// its own pending queue republishes this whole list, which is what gives
    /// the rows the identities a removal needs.
    pub(crate) queued_user_messages: VecDeque<QueuedPrompt>,
    /// The prompt this side already put in the transcript because the backend
    /// admitted it as a new turn. A backend that publishes its pending inbox
    /// keeps listing that prompt until the turn claims it, and every list it
    /// appears in would show the message a second time beside the row that is
    /// already there.
    pub(crate) published_prompt: Option<String>,
}

/// Child-agent activity the provider adapter reports for this conversation.
pub(crate) struct ChildAgents {
    /// Latest child-agent snapshot published by the provider adapter. The
    /// adapter owns child lifecycle; the pane keeps only this replacement
    /// copy so the right-side view never maintains a second mutable registry.
    pub(crate) background_tasks: Option<BackgroundTaskSnapshot>,
    /// Each child's own conversation, accumulated here rather than in the
    /// adapter so live activity is retained once and the retention bound
    /// applies to what is actually shown.
    pub(crate) transcripts: HashMap<BackgroundTaskKey, BackgroundTaskTranscript>,
    /// Claude session id whose child agents were already restored from
    /// history. Ready fires again during first-turn initialization, so the
    /// read happens once per conversation rather than once per confirmation.
    pub(crate) restored_session: Option<String>,
}
