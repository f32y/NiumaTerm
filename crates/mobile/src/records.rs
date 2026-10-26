//! Records the app renders. They are flat and final: the app shows them as
//! they are, and every protocol and view type is converted here, once, so
//! the desktop types never grow FFI attributes.

#[cfg(test)]
#[path = "records_tests.rs"]
mod records_tests;

use std::time::{Duration, Instant, UNIX_EPOCH};

use nmt_agent::chat::{Item, QuestionInput, QuestionMode};
use nmt_agent::session::AgentKind;
use nmt_agent::session::input::{QuestionError, QuestionStatus};
use nmt_agent::session::lifecycle::Status as LifecycleStatus;
use nmt_agent::session::view::{AgentView, DraftView, ViewEntry};
use nmt_remote::client::{LinkPath, PathPolicy};
use nmt_remote::connection::Status;
use nmt_remote_core::push::{PushEnvironment, PushKind, PushRegistration};
use nmt_remote_core::rpc::{
    EndReason, HostInfo, Origin, SessionInfo, SessionKind, SessionWorkspace,
};

use crate::commands::{command_records, skill_records};

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum HostStatus {
    /// Nothing needs the host, so nothing is connected.
    Idle,
    Connecting,
    Connected,
    /// The link dropped; views wait while it comes back.
    Reconnecting,
    /// The host no longer trusts this device; pairing again is the fix.
    Refused,
    /// Connecting failed a few times in a row; the app shows the host as
    /// unreachable until the user retries or the network changes.
    Unreachable,
}

impl From<Status> for HostStatus {
    fn from(status: Status) -> Self {
        match status {
            Status::Idle => Self::Idle,
            Status::Connecting => Self::Connecting,
            Status::Connected => Self::Connected,
            Status::Reconnecting => Self::Reconnecting,
            Status::Refused => Self::Refused,
            Status::Unreachable => Self::Unreachable,
        }
    }
}

/// How a connected host is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum HostLink {
    /// Directly, over the local network.
    Lan,
    Relay,
    /// Over the Internet without the relay, through a hole punched in both
    /// NATs.
    Direct,
}

impl From<&LinkPath> for HostLink {
    fn from(path: &LinkPath) -> Self {
        match path {
            LinkPath::Lan(_) => Self::Lan,
            LinkPath::Relay => Self::Relay,
            LinkPath::Direct(_) => Self::Direct,
        }
    }
}

/// Which network links to hosts may use, as the user chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum NetworkMode {
    /// The LAN when it reaches the host, otherwise the relay.
    Auto,
    Relay,
    Lan,
}

impl NetworkMode {
    pub(crate) fn policy(self) -> PathPolicy {
        match self {
            Self::Auto => PathPolicy::Auto,
            Self::Relay => PathPolicy::Relay,
            Self::Lan => PathPolicy::Lan,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct HostRecord {
    pub id: String,
    pub name: String,
    pub status: HostStatus,

    /// How the host is reached, while `status` is `Connected`.
    pub link: Option<HostLink>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SessionType {
    Terminal,
    Agent,
    /// A kind this build does not know, from a newer host.
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SessionRecord {
    pub id: String,
    pub title: String,
    pub kind: SessionType,

    /// The agent an agent session runs, as `host.info` names harnesses.
    pub harness: Option<String>,

    /// Started from a device instead of as a tab at the host.
    pub remote_origin: bool,

    /// The host workspace whose tab shows the session; none from hosts
    /// that do not say, and for sessions no tab shows.
    pub workspace: Option<SessionWorkspaceRecord>,

    /// A host tab still asleep: its shell or agent starts once someone
    /// opens it. Hosts from before the flag report every session running.
    pub pending: bool,
}

/// A host workspace, as the app groups a host's sessions by it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SessionWorkspaceRecord {
    /// Stable while the host runs, so a renamed workspace stays one group.
    pub id: String,

    pub name: String,

    /// Where the host lists the workspace, first at 0.
    pub position: u32,
}

impl From<SessionWorkspace> for SessionWorkspaceRecord {
    fn from(workspace: SessionWorkspace) -> Self {
        Self {
            id: workspace.id,
            name: workspace.name,
            position: workspace.position,
        }
    }
}

impl From<SessionInfo> for SessionRecord {
    fn from(info: SessionInfo) -> Self {
        Self {
            id: info.session,
            title: info.title,
            kind: match info.kind {
                SessionKind::Terminal => SessionType::Terminal,
                SessionKind::Agent => SessionType::Agent,
                SessionKind::Unknown => SessionType::Other,
            },
            harness: info.harness,
            remote_origin: info.origin == Origin::Remote,
            workspace: info.workspace.map(SessionWorkspaceRecord::from),
            pending: info.pending,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AgentProfileRecord {
    pub name: String,
    pub harness: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct WorkspaceRecord {
    /// The id sessions name this workspace by in their
    /// `SessionWorkspaceRecord`; none from hosts that send no ids, whose
    /// sessions match the workspace by name instead.
    pub id: Option<String>,

    pub name: String,
    pub path: String,
}

/// What this device may start on a host.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct HostOffer {
    pub agents: Vec<AgentProfileRecord>,
    pub workspaces: Vec<WorkspaceRecord>,
}

impl From<HostInfo> for HostOffer {
    fn from(info: HostInfo) -> Self {
        Self {
            agents: info
                .agents
                .into_iter()
                .map(|agent| AgentProfileRecord {
                    name: agent.name,
                    harness: agent.harness,
                })
                .collect(),
            workspaces: info
                .workspaces
                .into_iter()
                .map(|workspace| WorkspaceRecord {
                    id: workspace.id,
                    name: workspace.name,
                    path: workspace.path,
                })
                .collect(),
        }
    }
}

/// One transcript item, reduced to what a row shows.
#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
pub enum AgentItem {
    User {
        text: String,
        images: u32,
    },
    /// Markdown text.
    Agent {
        text: String,
    },
    Reasoning {
        summary: String,
    },
    Command {
        command: String,
        purpose: Option<String>,
        output: Option<String>,
        status: Option<String>,
        exit_code: Option<i64>,
    },
    FileChange {
        paths: String,
        diff: Option<String>,
        status: Option<String>,
    },
    Compaction {
        summary: Option<String>,
    },
    Tool {
        kind: String,
        title: String,
        output: Option<String>,
        status: Option<String>,
    },
    Error {
        text: String,
    },
    /// Something the conversation recorded that is not a message or tool
    /// call, shown as a one-line note.
    Notice {
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct AgentEntry {
    /// Position in the transcript. Entries change only from a splice point
    /// to the end, so a position keeps naming the same entry until then.
    pub index: u32,

    pub turn: u64,
    pub item: AgentItem,
}

pub(crate) fn agent_entry(index: usize, entry: &ViewEntry) -> AgentEntry {
    AgentEntry {
        index: index as u32,
        turn: entry.turn,
        item: agent_item(&entry.item, entry.images.len()),
    }
}

fn agent_item(item: &Item, images: usize) -> AgentItem {
    match item.clone() {
        Item::UserMessage { text } => AgentItem::User {
            text: text.unwrap_or_default(),
            images: images as u32,
        },
        Item::AgentMessage { text, .. } => AgentItem::Agent {
            text: text.unwrap_or_default(),
        },
        Item::Reasoning { summary, .. } => AgentItem::Reasoning {
            summary: summary.unwrap_or_default(),
        },
        Item::CommandExecution {
            command,
            purpose,
            aggregated_output,
            status,
            exit_code,
            ..
        } => AgentItem::Command {
            command,
            purpose,
            output: aggregated_output,
            status,
            exit_code,
        },
        Item::FileChange {
            paths,
            diff,
            status,
            ..
        } => AgentItem::FileChange {
            paths,
            diff,
            status,
        },
        Item::Compaction { detail, .. } => AgentItem::Compaction {
            summary: detail.summary,
        },
        Item::Other {
            kind,
            title,
            output,
            status,
            ..
        } => AgentItem::Tool {
            kind,
            title,
            output,
            status,
        },
        Item::Error { text } => AgentItem::Error { text },
        Item::TaskList { .. } => AgentItem::Notice {
            text: "Task list updated".into(),
        },
        Item::SideBoundary { message, .. } => AgentItem::Notice { text: message },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AgentPhase {
    Starting,
    Idle,
    Running,
    Exited,
}

/// Why this device no longer controls the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ViewEnd {
    /// The person at the host took it back; it keeps running there, and this
    /// device may take it again.
    TakenBack,
    /// The session is gone from the host.
    Closed,
    /// The host cannot be reached any more, or no longer trusts this device.
    Unreachable,
}

impl From<EndReason> for ViewEnd {
    fn from(reason: EndReason) -> Self {
        match reason {
            EndReason::Closed => Self::Closed,
            EndReason::TakenBack | EndReason::Unknown => Self::TakenBack,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ModelChoice {
    pub model: String,
    pub display: String,

    /// Reasoning efforts the model supports; empty when it has no control.
    pub efforts: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct RecentSession {
    pub id: String,
    pub title: String,
    pub cwd: Option<String>,
    pub last_active: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PendingApproval {
    pub description: String,

    /// An answer is on its way to the agent.
    pub submitted: bool,
}

/// How a question takes its answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AnswerInput {
    /// Only the listed options.
    SelectionOnly,
    /// The options, or typed text in their place.
    Text,
    /// Typed text that is never shown again, such as a password.
    Secret,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct QuestionChoice {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct QuestionItem {
    pub header: Option<String>,
    pub question: String,
    pub multi_select: bool,
    pub options: Vec<QuestionChoice>,
    pub input: AnswerInput,

    /// The answer as the host holds it, which a batch can arrive with.
    pub answer: QuestionAnswer,
}

/// One question's answer: the options picked, or typed text in their place.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct QuestionAnswer {
    /// Indexes into the question's options.
    pub selected: Vec<u32>,

    /// Typed text, which replaces the picked options when present.
    pub text: Option<String>,
}

/// Questions the agent asked together and waits on as one answer.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct QuestionBatch {
    pub id: String,
    pub questions: Vec<QuestionItem>,

    /// The answer is on its way to the agent.
    pub submitting: bool,

    /// Why the last answer did not reach the agent.
    pub error: Option<String>,

    /// How long until the host skips an optional batch nobody has started
    /// answering, measured on this device's clock.
    pub skips_in_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct QueuedMessage {
    /// Absent until the host has accepted the message into its queue; only
    /// accepted messages can be withdrawn.
    pub id: Option<String>,

    pub text: String,
}

/// A slash command the composer offers.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SlashCommandRecord {
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,

    /// Text typed after the name goes to the command.
    pub takes_arguments: bool,
}

/// A skill the composer offers.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SkillRecord {
    pub name: String,
    pub title: String,
    pub description: String,

    /// Where the harness found the skill, such as the user's or the
    /// workspace's skills, and for a name several skills share, the folder
    /// that holds this one, so their rows read apart.
    pub source: String,

    /// Tells apart skills that share a name; [`crate::agent::AgentHandle::submit`]
    /// takes it to invoke the one the person picked.
    pub path: String,

    /// A disabled skill is listed but cannot be invoked.
    pub enabled: bool,

    /// What the composer inserts to invoke the skill, `$name` or `/name`
    /// depending on the harness.
    pub token: String,
}

/// Everything besides the transcript that an agent screen shows.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct AgentState {
    /// The first snapshot arrived. Until then the screen has nothing to show.
    pub attached: bool,

    pub ended: Option<ViewEnd>,

    pub phase: AgentPhase,
    pub start_failure: Option<String>,

    /// A turn is running.
    pub working: bool,

    /// How long the running turn has taken, measured on this device's clock
    /// from the host's report.
    pub working_ms: Option<u64>,

    pub output_tokens: Option<u64>,
    pub detail: Option<String>,

    pub title: Option<String>,

    pub model: Option<String>,
    pub effort: Option<String>,
    pub models: Vec<ModelChoice>,
    pub plan_mode: bool,

    pub approval: Option<PendingApproval>,

    /// Question batches waiting for answers, oldest first.
    pub questions: Vec<QuestionBatch>,

    pub queue: Vec<QueuedMessage>,

    pub context_used: Option<u64>,
    pub context_window: Option<u64>,

    pub commands: Vec<SlashCommandRecord>,
    pub skills: Vec<SkillRecord>,
    pub recent_sessions: Vec<RecentSession>,
    pub history_loading: bool,
}

pub(crate) fn agent_state(
    view: Option<&AgentView>,
    turn_started: Option<Instant>,
    ended: Option<ViewEnd>,
) -> AgentState {
    let Some(view) = view else {
        return AgentState {
            attached: false,
            ended,
            phase: AgentPhase::Starting,
            start_failure: None,
            working: false,
            working_ms: None,
            output_tokens: None,
            detail: None,
            title: None,
            model: None,
            effort: None,
            models: Vec::new(),
            plan_mode: false,
            approval: None,
            questions: Vec::new(),
            queue: Vec::new(),
            context_used: None,
            context_window: None,
            commands: Vec::new(),
            skills: Vec::new(),
            recent_sessions: Vec::new(),
            history_loading: false,
        };
    };

    let slots = &view.slots;
    let status = &slots.status;
    let usage = slots.usage.context_window_usage;

    AgentState {
        attached: true,
        ended,
        phase: match status.status {
            LifecycleStatus::Starting => AgentPhase::Starting,
            LifecycleStatus::Idle => AgentPhase::Idle,
            LifecycleStatus::Running => AgentPhase::Running,
            LifecycleStatus::Exited => AgentPhase::Exited,
        },
        start_failure: status.start_failure.clone(),
        working: status.active,
        working_ms: status
            .active
            .then_some(turn_started)
            .flatten()
            .map(|started| started.elapsed().as_millis() as u64),
        output_tokens: status.live.output_tokens,
        detail: status.live.detail.clone(),
        title: slots.naming.title.clone(),
        model: slots.settings.settings.model.clone(),
        effort: slots.settings.settings.effort.clone(),
        models: slots
            .settings
            .models
            .iter()
            .map(|model| ModelChoice {
                model: model.model.clone(),
                display: model.display.clone(),
                // Codex uses application-wide levels instead of reporting
                // them in each model entry. Other agents report their own.
                efforts: if slots.catalogs.kind == Some(AgentKind::Codex) {
                    ["low", "medium", "high", "xhigh", "max", "ultra"]
                        .map(str::to_owned)
                        .to_vec()
                } else {
                    model.efforts.clone()
                },
            })
            .collect(),
        plan_mode: slots.settings.plan_mode,
        approval: slots
            .pending
            .approval
            .as_ref()
            .map(|approval| PendingApproval {
                description: approval.description.clone(),
                submitted: approval.submitted,
            }),
        questions: slots
            .pending
            .drafts
            .iter()
            .filter(|draft| {
                matches!(
                    draft.status,
                    QuestionStatus::Pending | QuestionStatus::Submitting
                )
            })
            .map(question_batch)
            .collect(),
        queue: slots
            .queue
            .prompts
            .iter()
            .map(|prompt| QueuedMessage {
                id: prompt.id.clone(),
                text: prompt.text.clone(),
            })
            .collect(),
        context_used: usage.map(|usage| usage.used_tokens()),
        context_window: usage.and_then(|usage| usage.max_tokens),
        commands: command_records(view),
        skills: skill_records(view),
        recent_sessions: slots
            .history
            .iter()
            .map(|session| RecentSession {
                id: session.id.clone(),
                title: session.title.clone(),
                cwd: session.cwd.clone(),
                last_active: session
                    .last_active
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            })
            .collect(),
        history_loading: status.history_loading,
    }
}

fn question_batch(draft: &DraftView) -> QuestionBatch {
    // The host skips an untouched optional batch this long after asking.
    const OPTIONAL_WAIT: Duration = Duration::from_secs(120);

    QuestionBatch {
        id: draft.id.clone(),
        questions: draft
            .questions
            .iter()
            .enumerate()
            .map(|(index, question)| QuestionItem {
                header: question.header.clone(),
                question: question.question.clone(),
                multi_select: question.multi_select,
                options: question
                    .options
                    .iter()
                    .map(|option| QuestionChoice {
                        label: option.label.clone(),
                        description: option.description.clone(),
                    })
                    .collect(),
                input: match question.input {
                    QuestionInput::SelectionOnly => AnswerInput::SelectionOnly,
                    QuestionInput::Text => AnswerInput::Text,
                    QuestionInput::Secret => AnswerInput::Secret,
                },
                answer: QuestionAnswer {
                    selected: draft
                        .selected
                        .get(index)
                        .map(|picks| picks.iter().map(|pick| *pick as u32).collect())
                        .unwrap_or_default(),
                    text: draft
                        .custom
                        .get(index)
                        .copied()
                        .unwrap_or(false)
                        .then(|| draft.text.get(index).cloned().unwrap_or_default()),
                },
            })
            .collect(),
        submitting: draft.status == QuestionStatus::Submitting,
        error: draft.error.as_ref().map(|error| match error {
            QuestionError::Disconnected => {
                "The agent disconnected before the answer arrived.".to_owned()
            }
            QuestionError::Rejected(message) => message.clone(),
        }),
        skips_in_ms: (draft.mode == QuestionMode::Optional
            && !draft.touched
            && draft.status == QuestionStatus::Pending)
            .then(|| draft.started.instant())
            .flatten()
            .map(|started| OPTIONAL_WAIT.saturating_sub(started.elapsed()).as_millis() as u64),
    }
}

/// An event the phone can be told about while it is away from a host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum NotificationKind {
    TurnFinished,
    TurnFailed,
    Approval,
    Question,
}

impl From<NotificationKind> for PushKind {
    fn from(kind: NotificationKind) -> Self {
        match kind {
            NotificationKind::TurnFinished => PushKind::TurnFinished,
            NotificationKind::TurnFailed => PushKind::TurnFailed,
            NotificationKind::Approval => PushKind::Approval,
            NotificationKind::Question => PushKind::Question,
        }
    }
}

/// Where and how a host should push to this phone.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PushSettings {
    /// The forwarder that holds the APNs key for this app build.
    pub endpoint: String,

    /// The APNs device token, hex.
    pub token: String,

    /// Production for TestFlight and App Store builds, sandbox otherwise.
    pub production: bool,

    /// The 32-byte key the host seals pushes with, base64.
    pub key: String,

    pub kinds: Vec<NotificationKind>,
}

impl From<PushSettings> for PushRegistration {
    fn from(settings: PushSettings) -> Self {
        Self {
            endpoint: settings.endpoint,
            token: settings.token,
            environment: if settings.production {
                PushEnvironment::Production
            } else {
                PushEnvironment::Sandbox
            },
            key: settings.key,
            kinds: settings.kinds.into_iter().map(PushKind::from).collect(),
        }
    }
}
