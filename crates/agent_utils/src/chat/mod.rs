//! Backend-neutral vocabulary for agent chat sessions. Each agent backend
//! (Codex app-server, Claude Code stream-json) translates its protocol into
//! these types, so the chat UI renders one transcript model
//! and never touches protocol strings.

mod commands;
mod controls;
mod questions;
mod sessions;
mod usage;

use crate::background_task::{
    BackgroundTaskKey, BackgroundTaskSnapshot, BackgroundTaskTranscriptUpdate,
};
pub use crate::chat::commands::*;
pub use crate::chat::controls::*;
pub use crate::chat::questions::*;
pub use crate::chat::sessions::*;
pub use crate::chat::usage::*;
use crate::workflow::WorkflowSnapshot;

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
        questions: Option<Vec<Question>>,
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
                Self::AgentMessage {
                    text, questions, ..
                },
                Self::AgentMessage {
                    text: completed,
                    questions: completed_questions,
                    ..
                },
            ) => {
                if let Some(completed) = completed {
                    *text = Some(completed.clone());
                }
                if let Some(completed) = completed_questions {
                    *questions = Some(completed.clone());
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
    /// Replacement snapshot of the agent compositions this deployment offers,
    /// and the one this conversation was built from. An empty list means the
    /// deployment composes no presets and every conversation shares the host's
    /// own composition, which is nothing for a picker to offer.
    AgentPresets {
        presets: Vec<AgentPreset>,
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
    InputRequested(QuestionRequest),
    InputResolved {
        id: String,
        resolution: QuestionResolution,
    },
    InputSubmissionFailed {
        id: String,
        message: String,
    },
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
    /// The prompts this conversation can be branched in front of, newest
    /// first, answering one request for them. Backends that keep their history
    /// behind the connection report it here; where the history is a file this
    /// side can read, the list is read directly instead of asked for.
    ForkCheckpoints(Result<Vec<ForkCheckpoint>, String>),
    /// A process shared by several sessions stopped without a requested
    /// shutdown. The UI retains the conversation identity and visible content
    /// so a replacement process can resume it.
    HostExited {
        message: String,
    },
    Error {
        message: String,
        /// The handshake itself failed; the session will not become usable.
        fatal: bool,
    },
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
mod tests;
