//! Backend-neutral vocabulary for agent chat sessions. Each agent backend
//! (Codex app-server, Claude Code stream-json) translates its protocol into
//! these types, so the chat UI renders one transcript model
//! and never touches protocol strings.

use std::time::SystemTime;

use crate::background_task::{
    BackgroundTaskKey, BackgroundTaskSnapshot, BackgroundTaskTranscriptUpdate,
};
use crate::workflow::WorkflowSnapshot;

/// Thread settings a chat UI lets the user pick. Field meanings are
/// per-backend: Codex sends them as overrides on every `turn/start`;
/// Claude stores its permission mode in `approval` and applies changes via
/// control requests before the next message (`approvals_reviewer`, `sandbox`,
/// and `tier` unused).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThreadSettings {
    pub model: Option<String>,
    pub approval: Option<String>,
    pub approvals_reviewer: Option<String>,
    pub sandbox: Option<String>,
    pub effort: Option<String>,
    /// `None` is the normal tier: the model catalog only lists additional
    /// tiers, so normal is expressed as an explicit `serviceTier: null`
    /// (double-optional in the serialized payload — null resets, absent keeps).
    pub tier: Option<String>,
}

/// One selectable execution-permission preset a backend advertises.
///
/// A backend whose presets are fixed needs none of this, because the UI can
/// name them itself. This exists for one whose preset table is part of the
/// deployment, where a hard-coded list would offer values the deployment does
/// not serve and hide the ones it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalPreset {
    /// Submitted back verbatim when the user picks it.
    pub value: String,
    pub label: String,
    pub description: Option<String>,
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
    /// Normalized protocol name without the leading slash.
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
/// The catalog is revalidated before this reference is sent to the backend.
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

/// Why a context compaction ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionTrigger {
    /// The backend reached its own context threshold and compacted unprompted.
    Automatic,
    /// The user asked for it (`/compact`).
    Manual,
}

impl CompactionTrigger {
    pub fn label(self) -> &'static str {
        match self {
            CompactionTrigger::Automatic => "automatic",
            CompactionTrigger::Manual => "manual",
        }
    }
}

/// One finished context compaction: the conversation before it was replaced by
/// a summary. Every field is optional because backends report different subsets
/// live and in their persisted transcript, and the boundary is worth showing
/// even when only part of the accounting is known.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Compaction {
    pub trigger: Option<CompactionTrigger>,
    /// Context size that triggered the compaction.
    pub pre_tokens: Option<u64>,
    /// Context size the conversation continues from.
    pub post_tokens: Option<u64>,
    /// How many messages were folded into the summary.
    pub messages_summarized: Option<u64>,
    /// Extra instructions the user passed alongside a manual compaction.
    pub user_context: Option<String>,
    /// The summary the conversation continues from. Claude marks it visible in
    /// the transcript only, so it arrives on resume rather than live.
    pub summary: Option<String>,
}

/// A typed view of one transcript item, used for both started and completed
/// notifications. `Option` fields mean "absent in this payload — keep what
/// streaming already produced".
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// User text is present for persisted replay and optional for live echoes,
    /// which a UI that already rendered the submitted prompt can skip.
    UserMessage {
        text: Option<String>,
    },
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
        /// Short explanation shown in the collapsed work row. Backends leave
        /// this absent when they cannot describe the command reliably.
        purpose: Option<String>,
        aggregated_output: Option<String>,
        status: Option<String>,
        exit_code: Option<i64>,
    },
    FileChange {
        id: String,
        paths: String,
        /// Reviewable diff body when the provider exposes one (Claude:
        /// reconstructed from the edit-tool input; Codex: backend diffs).
        diff: Option<String>,
        status: Option<String>,
    },
    /// A context-compaction boundary: everything above it was replaced by the
    /// carried summary. It has no status — the record only exists once the
    /// compaction finished.
    Compaction {
        id: String,
        detail: Compaction,
    },
    /// Every other tool-call kind (mcpToolCall, webSearch, dynamicToolCall,
    /// …): kind + best-effort title, so no activity is invisible.
    Other {
        id: String,
        kind: String,
        title: String,
        /// The tool's result payload (search matches, fetched content, …),
        /// delivered with the completion event.
        output: Option<String>,
        status: Option<String>,
    },
    /// A provider failure stored as part of the conversation transcript.
    Error {
        text: String,
    },
}

impl Item {
    /// Stable provider identity for streaming updates. User and error rows are
    /// complete when created and therefore need no update identity.
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::AgentMessage { id, .. }
            | Self::Reasoning { id, .. }
            | Self::CommandExecution { id, .. }
            | Self::FileChange { id, .. }
            | Self::Compaction { id, .. }
            | Self::Other { id, .. } => Some(id),
            Self::UserMessage { .. } | Self::Error { .. } => None,
        }
    }

    /// Completed and total entries of an agent-published task list, for items
    /// that are one. Claude's `TodoWrite` restates the entire list on every
    /// call and carries it as a markdown checklist, so counting the checklist
    /// lines of the latest such item describes the plan the agent is on.
    pub fn task_tally(&self) -> Option<(u32, u32)> {
        let Self::Other {
            kind,
            output: Some(checklist),
            ..
        } = self
        else {
            return None;
        };

        if kind != "TodoWrite" {
            return None;
        }

        let tally = checklist
            .lines()
            .filter(|line| line.starts_with("- ["))
            .fold((0, 0), |(done, total), line| {
                (done + u32::from(line.starts_with("- [x]")), total + 1)
            });

        (tally.1 > 0).then_some(tally)
    }

    /// Fold an authoritative completed payload into transcript state without
    /// discarding streamed fields that the provider omitted at completion.
    /// Returns false when the payload belongs to another item kind or id.
    pub fn merge_completed(&mut self, completed: &Self) -> bool {
        if self.id().is_none() || self.id() != completed.id() {
            return false;
        }

        match (self, completed) {
            (
                Self::AgentMessage { text, .. },
                Self::AgentMessage {
                    text: completed, ..
                },
            ) => {
                if let Some(completed) = completed {
                    *text = Some(completed.clone());
                }
            }
            (
                Self::Reasoning { summary, .. },
                Self::Reasoning {
                    summary: completed, ..
                },
            ) => {
                if summary.as_deref().unwrap_or_default().is_empty()
                    && let Some(completed) = completed
                {
                    *summary = Some(completed.clone());
                }
            }
            (
                Self::CommandExecution {
                    purpose,
                    aggregated_output,
                    status,
                    exit_code,
                    ..
                },
                Self::CommandExecution {
                    purpose: completed_purpose,
                    aggregated_output: completed_output,
                    status: completed_status,
                    exit_code: completed_exit,
                    ..
                },
            ) => {
                if purpose.is_none()
                    && let Some(completed_purpose) = completed_purpose
                {
                    *purpose = Some(completed_purpose.clone());
                }
                if let Some(completed_output) = completed_output {
                    *aggregated_output = Some(completed_output.clone());
                }
                if let Some(completed_status) = completed_status {
                    *status = Some(completed_status.clone());
                }
                if completed_exit.is_some() {
                    *exit_code = *completed_exit;
                }
            }
            (
                Self::FileChange { diff, status, .. },
                Self::FileChange {
                    diff: completed_diff,
                    status: completed_status,
                    ..
                },
            ) => {
                if let Some(completed_diff) = completed_diff {
                    *diff = Some(completed_diff.clone());
                }
                if let Some(completed_status) = completed_status {
                    *status = Some(completed_status.clone());
                }
            }
            (
                Self::Other { output, status, .. },
                Self::Other {
                    output: completed_output,
                    status: completed_status,
                    ..
                },
            ) => {
                if let Some(completed_output) = completed_output {
                    *output = Some(completed_output.clone());
                }
                if let Some(completed_status) = completed_status {
                    *status = Some(completed_status.clone());
                }
            }
            (
                Self::Compaction { detail, .. },
                Self::Compaction {
                    detail: completed, ..
                },
            ) => *detail = completed.clone(),
            _ => return false,
        }

        true
    }
}

/// One image a message carries, already encoded. Harnesses differ in what
/// they want done with it - one reads a file, another takes the bytes inline -
/// so this carries the bytes and lets each adapter decide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageImage {
    pub bytes: Vec<u8>,
    /// IANA media type of `bytes`, for a harness that must declare it.
    pub media_type: String,
}

/// Which directories a session listing covers. A conversation is recorded
/// against the directory it ran in, and the tab that lists them is rooted in
/// one, so the two answers a list can give are "this one" and "every one".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionScope {
    #[default]
    CurrentDirectory,
    AllDirectories,
}

/// One resumable persisted session, for the history list an empty chat tab
/// shows above its composer. Ordered newest-first by `last_active`.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSummary {
    pub id: String,
    /// First user prompt of the session (or an id prefix when none exists).
    pub title: String,
    pub branch: Option<String>,
    /// Working directory the session ran in. Carried because a list can span
    /// directories, and resuming a session outside the current one has to
    /// happen where it worked. `None` for a source that does not record it.
    pub cwd: Option<String>,
    pub last_active: SystemTime,
    /// Why a search returned this row. Present only in a list produced by a
    /// content search, because the excerpt describes the query rather than the
    /// session, and an ordinary list has no query to describe.
    pub snippet: Option<String>,
}

/// One prompt the backend accepted but has not started working on, in the
/// order it will be claimed.
///
/// `id` is what a removal addresses. A backend that reports its pending work
/// without naming the individual messages leaves it absent, and the row is
/// then read-only rather than carrying a control that could not address
/// anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedPrompt {
    pub id: Option<String>,
    pub text: String,
}

impl QueuedPrompt {
    /// A prompt this side queued optimistically, before the backend has said
    /// anything about it.
    pub fn local(text: String) -> Self {
        Self { id: None, text }
    }
}

/// The session's standing objective, when the backend runs one.
///
/// A goal outlives the turn that created it and drives further turns on its
/// own, so it is session state rather than transcript content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoalStatus {
    pub objective: String,
    /// The backend's own lifecycle word for the goal, shown as it was given.
    pub phase: String,
    pub rounds_started: u64,
    pub max_rounds: u64,
}

/// Whole-log conversation counters, independent of how much history has been
/// paged in. Reported only by a backend that folds them from its complete log;
/// a count derived from the visible transcript would disagree with it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionStats {
    pub turns: u64,
    pub steps: u64,
    /// Summed model wall time over the steps that produced a message.
    pub model_ms: u64,
    /// Summed tool wall time over matched call/result pairs.
    pub tool_ms: u64,
}

/// Token accounting from one provider reporting scope. The total is
/// authoritative; optional categories describe parts of that total and stay
/// absent when a protocol does not expose them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenUsageBreakdown {
    pub total_tokens: u64,
    pub input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
}

impl TokenUsageBreakdown {
    /// A compaction boundary can report the replacement size without enough
    /// information to attribute tokens to categories.
    pub const fn total_only(total_tokens: u64) -> Self {
        Self {
            total_tokens,
            input_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            output_tokens: None,
            reasoning_output_tokens: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextUsageScope {
    Thread,
    LastTurn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScopedTokenUsage {
    pub scope: ContextUsageScope,
    pub breakdown: TokenUsageBreakdown,
}

/// Latest replacement snapshot of active context usage and any cumulative
/// accounting that the same provider update can identify precisely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextWindowUsage {
    pub current: TokenUsageBreakdown,
    pub cumulative: Option<ScopedTokenUsage>,
    pub max_tokens: Option<u64>,
}

impl ContextWindowUsage {
    pub const fn used_tokens(self) -> u64 {
        self.current.total_tokens
    }
}

/// One labelled part of what currently fills the context window, such as the
/// system prompt, the tool definitions, or the conversation itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextSegment {
    pub label: String,
    pub tokens: u64,
    /// Colour the provider suggests for this segment, as it writes it. Kept as
    /// the provider's own string because a UI may prefer its theme instead.
    pub color: Option<String>,
    /// The segment is reserved rather than occupied: counted against the
    /// window, but holding no conversation content yet.
    pub deferred: bool,
}

/// How the context window is currently filled, as opposed to how tokens were
/// billed. A provider that only reports accounting never publishes this, so
/// its absence is a normal state rather than a failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextComposition {
    pub segments: Vec<ContextSegment>,
    pub used_tokens: u64,
    /// Window the provider measures against. This can be smaller than the
    /// model's own window when the provider reserves room to compact.
    pub max_tokens: Option<u64>,
    /// The model's window before any such reserve, when the provider
    /// distinguishes the two.
    pub raw_max_tokens: Option<u64>,
    /// Where automatic compaction takes over, when the provider reports it.
    pub auto_compact_threshold: Option<u64>,
}

/// Something a turn is doing that produces no output while it lasts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnActivity {
    /// A provider request failed and is being tried again. The turn is waiting
    /// rather than working, which is otherwise indistinguishable: elapsed time
    /// climbs the same either way and the token count sits still for both.
    Retrying {
        attempt: u64,
        total: u64,
        /// The provider's own account of the failure, already user-facing.
        reason: String,
    },
}

/// One selectable answer of a [`Question`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionOption {
    /// Sent back verbatim as the answer; the provider matches on this text.
    pub label: String,
    pub description: Option<String>,
}

/// A choice the model needs from the user before it can continue. The provider
/// caps a batch at four questions of two to four options each, so the card that
/// renders these never needs scrolling or paging.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    /// Short chip label above the question text.
    pub header: Option<String>,
    /// Full question text. It is also the key the answer is reported under, so
    /// it must survive the round trip unmodified.
    pub question: String,
    /// Whether more than one option may be chosen.
    pub multi_select: bool,
    pub options: Vec<QuestionOption>,
}

/// What a chat UI needs to react to, in transcript order.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Handshake finished; carries the thread's effective settings so the UI
    /// can seed its pickers with real values.
    Ready(ThreadSettings),
    Models(Vec<ModelInfo>),
    /// The backend refused an effort change, carrying the level the session
    /// stays on. A refused setting is not something the conversation said, so
    /// it is reported beside the control that asked for it rather than as a
    /// transcript entry.
    EffortRejected {
        message: String,
        effort: Option<String>,
    },
    /// Replacement snapshot of the execution-permission presets this thread can
    /// switch between, and the one it is on. Reported only by a backend whose
    /// preset table belongs to its deployment rather than to this UI.
    ApprovalPresets {
        presets: Vec<ApprovalPreset>,
        current: Option<String>,
    },
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
    /// Replacement output-token count for the active turn.
    TurnOutputTokensUpdated(u64),
    /// Replacement snapshot of the current thread's active context window.
    ContextWindowUpdated(ContextWindowUsage),
    /// Replacement breakdown of what fills that window. Reported only by
    /// providers that measure their own context composition.
    ContextCompositionUpdated(ContextComposition),
    /// The backend started rewriting the conversation to reclaim context. Turn
    /// output stops until it finishes, so this drives a progress indicator; the
    /// finished boundary arrives separately as [`Item::Compaction`].
    CompactionStarted,
    /// Compaction ended. A failure is worth surfacing because the turn that
    /// triggered it usually dies next with an over-length prompt.
    CompactionFinished {
        error: Option<String>,
    },
    /// A file-only rewind control request finished. It is not a model turn and
    /// therefore has no transcript item or turn lifecycle of its own.
    FileRewindCompleted {
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
    /// The model asked the user to choose between options before continuing;
    /// answer with the session's `respond_questions`.
    QuestionsRequested {
        questions: Vec<Question>,
    },
    /// The pending questions were answered or cleared by turn lifecycle.
    QuestionsResolved,
    /// Replacement snapshot of the child agents this session spawned. Child
    /// lifecycle is reduced by the adapter, so this never affects the parent
    /// transcript, turn state, or approvals.
    BackgroundTasks(BackgroundTaskSnapshot),
    /// Replacement snapshot of this session's workflow runs. Workflow agents
    /// are not child agents, so these rows never reach `Background Tasks` and
    /// never affect the parent transcript or turn state.
    Workflows(WorkflowSnapshot),
    /// One workflow agent's own conversation, read from its persisted
    /// transcript. Delivered separately from the run snapshot because it is
    /// read only while someone has that agent open.
    WorkflowAgentTranscript {
        task_id: String,
        agent_id: String,
        items: Vec<Item>,
    },
    /// One child's own conversation, in the same items the parent transcript
    /// uses. Delivered separately from the summary snapshot because a child's
    /// content is only worth carrying once someone is reading it.
    BackgroundTaskTranscript {
        key: BackgroundTaskKey,
        update: BackgroundTaskTranscriptUpdate,
    },
    /// Resumable sessions for the tab's working directory, newest first.
    History(Vec<SessionSummary>),
    /// The conversations one content search matched, in the backend's own rank
    /// order. Separate from [`Self::History`] because history pages accumulate
    /// while an answer to a query replaces what an earlier query answered.
    SessionSearchResults(Vec<SessionSummary>),
    /// Replacement snapshot of the prompts the backend has accepted but not
    /// started. Reported only by a backend that owns the pending queue itself;
    /// where the queue is this side's own bookkeeping, sending it back would
    /// overwrite what this side already knows with a copy of it.
    QueuedPrompts(Vec<QueuedPrompt>),
    /// Replacement value of the session's standing objective. `None` is a
    /// session with no goal, which is also what a backend that runs none
    /// reports by never sending this.
    GoalUpdated(Option<GoalStatus>),
    /// Whether the backend is currently collaborating on a plan rather than
    /// carrying out work.
    PlanModeUpdated(bool),
    /// A name for this conversation, for whatever shows it in a list. Backends
    /// differ in where it comes from — one summarizes the conversation with a
    /// model call, another is told what to call it — so this reports the
    /// settled name rather than the material for one.
    TitleUpdated(String),
    /// Replacement whole-log conversation counters.
    SessionStatsUpdated(SessionStats),
    /// Reconstructed transcript of a resumed session, to pre-fill the UI.
    Replay(Vec<ReplayTurn>),
    /// What the running turn is doing beyond producing output, when the backend
    /// reports something the elapsed time and token count cannot show. `None`
    /// clears it. The words belong to the view: an adapter reports the facts it
    /// was given and does not know the reader's language.
    StatusDetail(Option<TurnActivity>),
    Error {
        message: String,
        /// The handshake itself failed; the session will not become usable.
        fatal: bool,
    },
}

/// One turn of a resumed conversation. A live turn's shape comes from the turn
/// lifecycle events — where it started, how long it ran, what it cost, whether
/// the user stopped it — none of which a flat list of items can express, so a
/// replay that dropped it left restored conversations unfoldable and without
/// their durations. Every field is optional because providers persist
/// different parts of it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReplayTurn {
    pub items: Vec<ReplayItem>,
    /// Wall time the turn took.
    pub seconds: Option<u64>,
    /// Output tokens the turn produced.
    pub output_tokens: Option<u64>,
    /// The user stopped the turn before it finished.
    pub interrupted: bool,
}

/// One entry of a resumed turn, with the wall-clock time the provider recorded
/// for it as Unix seconds. Formatting belongs to the UI, which owns the
/// viewer's time zone.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayItem {
    pub item: Item,
    pub at: Option<i64>,
}

impl ReplayTurn {
    /// A turn carrying items but none of the accounting, for a provider that
    /// persists no turn metadata.
    pub fn from_items(items: impl IntoIterator<Item = Item>) -> Self {
        Self {
            items: items
                .into_iter()
                .map(|item| ReplayItem { item, at: None })
                .collect(),
            ..Self::default()
        }
    }
}

/// Outcome of a session's `send_user_message`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    /// A new turn was started.
    StartedTurn,
    /// The message was steered into the already-running turn.
    Steered,
    /// The handshake has not produced a thread yet.
    NotReady,
    /// The backend understood the message and refused it, in its own words.
    /// Only backends that admit a prompt over a request-response call can tell
    /// the difference between this and [`Self::NotReady`]; one that writes to a
    /// pipe learns nothing at send time and never reports it.
    Rejected { message: String },
}

#[cfg(test)]
mod tests {
    use super::Item;

    #[test]
    fn completed_items_merge_without_erasing_streamed_fields() {
        let mut command = Item::CommandExecution {
            id: "command-1".into(),
            command: "cargo test".into(),
            purpose: Some("Run focused tests".into()),
            aggregated_output: Some("streamed output".into()),
            status: Some("inProgress".into()),
            exit_code: None,
        };
        let completed = Item::CommandExecution {
            id: "command-1".into(),
            command: "cargo test".into(),
            purpose: None,
            aggregated_output: None,
            status: Some("completed".into()),
            exit_code: Some(0),
        };

        assert!(command.merge_completed(&completed));
        assert_eq!(
            command,
            Item::CommandExecution {
                id: "command-1".into(),
                command: "cargo test".into(),
                purpose: Some("Run focused tests".into()),
                aggregated_output: Some("streamed output".into()),
                status: Some("completed".into()),
                exit_code: Some(0),
            }
        );
    }

    #[test]
    fn completed_reasoning_is_only_a_fallback_for_missing_stream_text() {
        let mut streamed = Item::Reasoning {
            id: "reasoning-1".into(),
            summary: Some("streamed".into()),
        };
        let completed = Item::Reasoning {
            id: "reasoning-1".into(),
            summary: Some("completed".into()),
        };
        assert!(streamed.merge_completed(&completed));
        assert_eq!(
            streamed,
            Item::Reasoning {
                id: "reasoning-1".into(),
                summary: Some("streamed".into())
            }
        );

        let mut missing = Item::Reasoning {
            id: "reasoning-1".into(),
            summary: None,
        };
        assert!(missing.merge_completed(&completed));
        assert_eq!(missing, completed);
    }
}
