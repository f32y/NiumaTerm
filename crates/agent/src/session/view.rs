//! A serializable projection of one conversation, for views that run in
//! another process than the session.
//!
//! The host keeps running the backend and the [`SessionController`]; a view
//! elsewhere holds a replica controller without a backend and applies what
//! the host publishes. The projection is split into a transcript, replaced
//! from its first changed entry onwards, and slots, each replaced whole when
//! it changes. Slots are coarse because they are small: comparing and
//! resending a whole slot is cheaper to get right than diffing inside one.
//!
//! Times are sent as durations measured on the host, so the two clocks never
//! have to agree.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::chat::{
    AgentPreset, ApprovalPreset, ContextComposition, ContextWindowUsage, Item, ModelInfo, Question,
    QuestionMode, QueuedPrompt, SessionStats, SessionSummary, SkillCatalog, SlashCommandInfo,
    ThreadSettings,
};
use crate::progress::{GoalStatus, TaskList};
use crate::session::AgentKind;
use crate::session::branch::BranchPicker;
use crate::session::commands::PendingSlashCommand;
use crate::session::controller::SessionController;
use crate::session::input::{QuestionError, QuestionKey, QuestionStatus};
use crate::session::lifecycle::Status;
use crate::transcript::TranscriptEntry;
use crate::transcript::conversation::{ConversationImage, EntryMetadata};
use crate::transcript::turns::{GenerationStats, TurnLedger};

/// Everything a view needs to render the conversation at one moment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentView {
    pub transcript: Vec<ViewEntry>,
    pub slots: ViewSlots,
}

/// One transcript entry. Image bytes travel separately, by reference, so an
/// entry resent while its text streams does not resend its images.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewEntry {
    pub turn: u64,
    pub item: Item,
    pub at: Option<i64>,
    pub images: Vec<ImageRef>,
}

impl ViewEntry {
    /// The entry as a replica's transcript holds it, with its images once
    /// the view has them.
    pub fn into_entry(self, images: Vec<Arc<ConversationImage>>) -> TranscriptEntry<EntryMetadata> {
        TranscriptEntry {
            turn: self.turn,
            item: self.item,
            metadata: EntryMetadata {
                at: self.at,
                images,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRef {
    pub id: Uuid,
    pub len: u64,
}

/// The command a view fetches an image's bytes with, by [`ImageRef`].
pub const IMAGE_METHOD: &str = "image";

/// An image's bytes as they travel to a view.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageData {
    #[serde(with = "crate::session::command::base64_bytes")]
    pub bytes: Arc<[u8]>,
}

/// A time on the host, sent as how long ago it was. Two readings compare
/// equal when both are set or both are not: the value advances on its own,
/// and only its appearance or removal is a change that gets published.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Since(pub Option<Duration>);

impl Since {
    pub fn of(at: Option<Instant>) -> Self {
        Self(at.map(|at| at.elapsed()))
    }

    /// The same moment on this machine's clock.
    pub fn instant(self) -> Option<Instant> {
        let now = Instant::now();

        self.0.map(|ago| now.checked_sub(ago).unwrap_or(now))
    }
}

impl PartialEq for Since {
    fn eq(&self, other: &Self) -> bool {
        self.0.is_some() == other.0.is_some()
    }
}

/// The running turn, as [`crate::transcript::turns::LiveTurn`] holds it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveView {
    pub started: Since,
    pub output_tokens: Option<u64>,
    pub detail: Option<String>,
    pub compacting: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatusView {
    pub status: Status,
    pub epoch: u64,
    pub start_failure: Option<String>,
    pub turn: u64,
    pub active: bool,
    pub live: LiveView,
    pub submitted_at: Since,
    pub first_output_latency: Option<Duration>,
    pub last_response_at: Since,
    #[serde(default)]
    pub history_loading: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageView {
    pub context_window_usage: Option<ContextWindowUsage>,
    pub context_composition: Option<ContextComposition>,
    pub session_stats: Option<SessionStats>,
    pub generation: GenerationStats,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SettingsView {
    pub settings: ThreadSettings,
    pub models: Vec<ModelInfo>,
    pub approval_presets: Vec<ApprovalPreset>,
    pub agent_presets: Vec<AgentPreset>,
    pub plan_mode: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CatalogView {
    pub commands: Option<Vec<SlashCommandInfo>>,
    pub skills: Option<SkillCatalog>,

    /// The harness, which decides the commands its adapter adds to these and
    /// how a slash line routes. Absent from hosts that predate it.
    #[serde(default)]
    pub kind: Option<AgentKind>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApprovalView {
    pub description: String,
    pub submitted: bool,
}

/// One question batch with the answers typed into it so far.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DraftView {
    pub id: String,
    pub questions: Vec<Question>,
    pub mode: QuestionMode,
    pub status: QuestionStatus,
    pub error: Option<QuestionError>,
    pub selected: Vec<Vec<usize>>,
    pub text: Vec<String>,
    pub custom: Vec<bool>,
    pub key: QuestionKey,
    pub started: Since,
    pub touched: bool,
}

/// The answers typed into one question batch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DraftAnswers {
    pub selected: Vec<Vec<usize>>,
    pub text: Vec<String>,
    pub custom: Vec<bool>,
}

/// What the conversation waits on the user for.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PendingView {
    pub epoch: u64,
    pub sequence: u64,
    pub disconnected: bool,
    pub approval: Option<ApprovalView>,
    pub drafts: Vec<DraftView>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QueueView {
    pub prompts: VecDeque<QueuedPrompt>,
    pub commands: VecDeque<PendingSlashCommand>,
    pub awaiting_turn: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TasksView {
    pub list: Option<TaskList>,
    pub snapshots: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewSlots {
    pub status: StatusView,
    pub usage: UsageView,
    pub turns: TurnLedger,
    pub settings: SettingsView,
    pub catalogs: CatalogView,
    pub pending: PendingView,
    pub queue: QueueView,
    pub goal: Option<GoalStatus>,
    pub tasks: TasksView,

    pub naming: NamingView,

    /// The branch picker, which every view shows while one is open.
    pub branch: BranchPicker,

    /// Conversations the host listed for a view in another process.
    pub history: Vec<SessionSummary>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NamingView {
    /// Whether the conversation has its title, so a prompt from any view
    /// knows not to name it again.
    pub named: bool,

    pub title: Option<String>,
}

/// One slot, replacing its previous value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "key", content = "value", rename_all = "snake_case")]
pub enum ViewSlot {
    Status(StatusView),
    Usage(UsageView),
    Turns(TurnLedger),
    Settings(SettingsView),
    Catalogs(CatalogView),
    Pending(PendingView),
    Queue(QueueView),
    Goal(Option<GoalStatus>),
    Tasks(TasksView),
    Naming(NamingView),
    Branch(BranchPicker),
    History(Vec<SessionSummary>),
}

impl AgentView {
    /// The view as operations that rebuild it from nothing: the whole
    /// transcript, then every slot.
    pub fn into_ops(self) -> Vec<ViewOp> {
        let mut ops = vec![ViewOp::Splice {
            from: 0,
            entries: self.transcript,
        }];

        ops.extend(
            self.slots
                .into_slots()
                .into_iter()
                .map(|slot| ViewOp::Slot {
                    slot: Box::new(slot),
                }),
        );

        ops
    }
}

impl AgentView {
    /// Apply one published change. This is the inverse of what
    /// [`ViewPublisher`] emits, for a view that renders the projection
    /// directly instead of running a replica controller. A splice past the
    /// end appends, so a view that fell behind by a trimmed entry still
    /// converges on the next splice instead of panicking.
    pub fn apply(&mut self, op: ViewOp) {
        match op {
            ViewOp::Splice { from, entries } => {
                self.transcript.truncate(from);
                self.transcript.extend(entries);
            }
            ViewOp::Slot { slot } => self.slots.set(*slot),
        }
    }
}

impl ViewSlots {
    /// Replace the slot `slot` names.
    pub fn set(&mut self, slot: ViewSlot) {
        match slot {
            ViewSlot::Status(value) => self.status = value,
            ViewSlot::Usage(value) => self.usage = value,
            ViewSlot::Turns(value) => self.turns = value,
            ViewSlot::Settings(value) => self.settings = value,
            ViewSlot::Catalogs(value) => self.catalogs = value,
            ViewSlot::Pending(value) => self.pending = value,
            ViewSlot::Queue(value) => self.queue = value,
            ViewSlot::Goal(value) => self.goal = value,
            ViewSlot::Tasks(value) => self.tasks = value,
            ViewSlot::Naming(value) => self.naming = value,
            ViewSlot::Branch(value) => self.branch = value,
            ViewSlot::History(value) => self.history = value,
        }
    }

    /// Every slot, for a view starting from nothing.
    pub(crate) fn into_slots(self) -> Vec<ViewSlot> {
        vec![
            ViewSlot::Status(self.status),
            ViewSlot::Usage(self.usage),
            ViewSlot::Turns(self.turns),
            ViewSlot::Settings(self.settings),
            ViewSlot::Catalogs(self.catalogs),
            ViewSlot::Pending(self.pending),
            ViewSlot::Queue(self.queue),
            ViewSlot::Goal(self.goal),
            ViewSlot::Tasks(self.tasks),
            ViewSlot::Naming(self.naming),
            ViewSlot::Branch(self.branch),
            ViewSlot::History(self.history),
        ]
    }

    /// The slots that differ from `previous`.
    fn changed_since(&self, previous: &Self) -> Vec<ViewSlot> {
        let mut changed = Vec::new();

        if self.status != previous.status {
            changed.push(ViewSlot::Status(self.status.clone()));
        }

        if self.usage != previous.usage {
            changed.push(ViewSlot::Usage(self.usage.clone()));
        }

        if self.turns != previous.turns {
            changed.push(ViewSlot::Turns(self.turns.clone()));
        }

        if self.settings != previous.settings {
            changed.push(ViewSlot::Settings(self.settings.clone()));
        }

        if self.catalogs != previous.catalogs {
            changed.push(ViewSlot::Catalogs(self.catalogs.clone()));
        }

        if self.pending != previous.pending {
            changed.push(ViewSlot::Pending(self.pending.clone()));
        }

        if self.queue != previous.queue {
            changed.push(ViewSlot::Queue(self.queue.clone()));
        }

        if self.goal != previous.goal {
            changed.push(ViewSlot::Goal(self.goal.clone()));
        }

        if self.tasks != previous.tasks {
            changed.push(ViewSlot::Tasks(self.tasks.clone()));
        }

        if self.naming != previous.naming {
            changed.push(ViewSlot::Naming(self.naming.clone()));
        }

        if self.branch != previous.branch {
            changed.push(ViewSlot::Branch(self.branch.clone()));
        }

        if self.history != previous.history {
            changed.push(ViewSlot::History(self.history.clone()));
        }

        changed
    }
}

/// A change to a view.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ViewOp {
    /// Replace every entry from `from` to the end. One rule covers new
    /// entries, streamed text, completions, and a cleared or trimmed
    /// transcript.
    Splice {
        from: usize,
        entries: Vec<ViewEntry>,
    },
    Slot {
        slot: Box<ViewSlot>,
    },
}

/// Turns a controller's state into view operations, remembering what it
/// published last so only changes go out.
#[derive(Default)]
pub struct ViewPublisher {
    version: (u64, u64),
    slots: Option<ViewSlots>,
}

impl ViewPublisher {
    /// The whole view, which later [`Self::changes`] build on.
    pub fn snapshot(&mut self, controller: &SessionController) -> AgentView {
        let slots = controller.view_slots();

        self.version = controller.conversation().borrow().version();
        self.slots = Some(slots.clone());

        AgentView {
            transcript: controller.transcript_view(0),
            slots,
        }
    }

    /// What changed since the last snapshot or changes.
    pub fn changes(&mut self, controller: &SessionController) -> Vec<ViewOp> {
        let mut ops = Vec::new();

        let change = controller
            .conversation()
            .borrow()
            .changes_since(self.version);

        if let Some(change) = change {
            let conversation = controller.conversation().borrow();
            let from = change.first.min(conversation.content.entries().len());

            self.version = conversation.version();

            drop(conversation);

            ops.push(ViewOp::Splice {
                from,
                entries: controller.transcript_view(from),
            });
        }

        let slots = controller.view_slots();

        match &self.slots {
            Some(previous) => {
                ops.extend(
                    slots
                        .changed_since(previous)
                        .into_iter()
                        .map(|slot| ViewOp::Slot {
                            slot: Box::new(slot),
                        }),
                )
            }
            None => ops.extend(
                slots
                    .clone()
                    .into_slots()
                    .into_iter()
                    .map(|slot| ViewOp::Slot {
                        slot: Box::new(slot),
                    }),
            ),
        }

        self.slots = Some(slots);

        ops
    }
}
