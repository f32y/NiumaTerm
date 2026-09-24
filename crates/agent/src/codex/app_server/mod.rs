//! Codex `app-server` chat session: process lifecycle, JSON-RPC handshake,
//! and translation of the backend protocol into typed events for a chat UI.
//!
//! The app-server protocol is Codex's supported integration surface for
//! third-party UIs (it powers the VS Code extension). Each `Session` owns one
//! conversation thread and shares its app-server host with other sessions.

pub use crate::background_task::{
    BackgroundTaskKey, BackgroundTaskLoadState, BackgroundTaskTranscriptUpdate,
};
pub use crate::chat::{
    Compaction, CompactionTrigger, ContextUsageScope, ContextWindowUsage, Event, ForkAnchor,
    ForkCheckpoint, Item, ModelInfo, ScopedTokenUsage, SendOutcome, SessionScope, SessionSummary,
    SkillCatalog, SkillInfo, SkillReference, SlashCommandArguments, SlashCommandInfo,
    SlashCommandOutcome, SlashCommandRunPolicy, SlashCommandSource, ThreadSettings,
    TokenUsageBreakdown,
};
pub use crate::codex::app_server::side::SideStart;

pub(crate) use crate::codex::app_server::title_generation::provisional_title_from_prompt;

mod background_tasks;
mod compaction;
mod control;
mod conversation;
mod host;
mod progress;
mod protocol;
mod questions;
mod side;
mod skills;
mod team;
mod title_generation;

#[cfg(test)]
#[cfg(windows)]
mod steering_tests;
#[cfg(test)]
mod tests;

use std::mem::take;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
#[cfg(test)]
use std::time::UNIX_EPOCH;

use futures::future::{BoxFuture, FutureExt as _, ready};
use serde_json::{Value, json};

use crate::LaunchConfig;
use crate::chat::{QuestionRequest, QuestionResponse, TeamDecisionRequest};
use crate::codex::ProviderConfig;
use crate::codex::app_server::background_tasks::{CodexTasks, ThreadScope, notification_thread_id};
use crate::codex::app_server::control::{ControlOperation, ControlState, QueryKind};
use crate::codex::app_server::conversation::ThreadState;
#[cfg(test)]
use crate::codex::app_server::conversation::TurnOutputUsage;
use crate::codex::app_server::host::{CodexHost, HOST_EXIT_METHOD, RegistrationId};
use crate::codex::app_server::progress::{goal_status, spawn_plan_restore};
#[cfg(test)]
use crate::codex::app_server::protocol::thread_start_params;
use crate::codex::app_server::protocol::{
    CodexCommand, codex_user_input, file_change_paths, initial_thread_request,
    parse_fork_checkpoints, parse_models, parse_replay, parse_thread_settings,
    parse_thread_summaries, resumed_thread_events, skills_list_request, stringify_command,
    thread_list_params, thread_name_request, thread_resume_params, turn_interrupt_request,
    turn_start_params,
};
use crate::codex::app_server::questions::{
    on_question_request, on_question_response, respond_input, restore_question_requests,
};
use crate::codex::app_server::side::{
    side_boundary_item, side_boundary_request, side_fork_request,
};
#[cfg(test)]
use crate::codex::app_server::skills::parse_skill_catalog;
use crate::codex::app_server::skills::{SkillRefreshState, skill_catalog_from_response};
use crate::codex::app_server::team::{TeamState, decision_tool};
use crate::codex::app_server::title_generation::{
    TITLE_GENERATION_RESULT_METHOD, TitleGenerationHandle, TitleGenerationRequest,
    parse_title_generation_result, start_title_generation,
};
use crate::session::team_capabilities::{ModeratorAdmission, RecoveredTeamTurn, TeamLaunch};
use crate::session::{AgentKind, ConversationTitleRequest};
use crate::subprocess::DROP_SHUTDOWN_GRACE;
use crate::workspace::AgentWorkspace;

const FIRST_TURN_RPC_ID: u64 = 100;
const PROVIDER_API_FIELD: &str = concat!("wi", "re_api");

/// First page size for the history list; enough to fill the visible list
/// several times over. `nextCursor` remains available for deeper paging.
const THREAD_LIST_LIMIT: u64 = 50;

/// Notifications that describe one thread's activity. They are routed by
/// thread id before parent handling, so a descendant's turn or item can never
/// change the parent's turn identity, running state, or transcript.
const THREAD_SCOPED_NOTIFICATIONS: [&str; 14] = [
    "turn/started",
    "turn/completed",
    "thread/status/changed",
    "thread/tokenUsage/updated",
    "item/started",
    "item/completed",
    "item/agentMessage/delta",
    "item/reasoning/summaryTextDelta",
    "item/reasoning/textDelta",
    "item/commandExecution/outputDelta",
    "error",
    "turn/plan/updated",
    "thread/goal/updated",
    "thread/goal/cleared",
];

#[derive(Clone, Debug, Default)]
struct ThreadProfile {
    model: Option<String>,
    provider: Option<ProviderConfig>,
}

impl From<&LaunchConfig> for ThreadProfile {
    fn from(launch: &LaunchConfig) -> Self {
        Self {
            model: launch.model.clone(),
            provider: launch.provider.clone(),
        }
    }
}

type SessionDelivery = Arc<dyn Fn(Value) + Send + Sync>;

pub struct Session {
    host: Option<Arc<CodexHost>>,
    conversation: ThreadState,
    registration_id: RegistrationId,
    deliver: SessionDelivery,
    detached: bool,
    control: ControlState,
    next_title_generation_id: u64,
    title_generation: Option<TitleGenerationHandle>,

    /// Cursor for the next history page; `None` once the final page arrived.
    history_cursor: Option<String>,

    history_scope: SessionScope,
    skill_refresh: SkillRefreshState,

    /// Profile-level model/provider overrides reused for thread start, history
    /// filtering, and resume. Provider credentials remain only in process env.
    thread_profile: ThreadProfile,

    /// The directories this conversation was started with. Held for the life
    /// of the session so every turn declares the same writable roots the
    /// thread was opened with.
    workspace: AgentWorkspace,

    initial_resume: Option<String>,
    suppress_resume_replay: bool,

    /// Descendant-thread tracking for the `Background Tasks` view.
    background: CodexTasks,

    team: Option<TeamState>,

    /// The parent this session forks from when it is a side conversation.
    side: Option<Box<SideStart>>,

    /// A side conversation's settings from its fork reply, held until the
    /// boundary is written: a question sent before the boundary would be
    /// read as a continuation of the parent's inherited task.
    side_ready: Option<Box<ThreadSettings>>,
}

#[derive(Default)]
struct ConversationStart {
    resume: Option<String>,
    suppress_replay: bool,
    team: Option<TeamLaunch>,
    side: Option<SideStart>,
}

impl Session {
    pub fn adapter_commands() -> Vec<SlashCommandInfo> {
        vec![
            SlashCommandInfo {
                name: "compact".to_string(),
                description: "Compact the current conversation context".to_string(),
                argument_hint: None,
                source: SlashCommandSource::Adapter,
                arguments: SlashCommandArguments::None,
                run_policy: SlashCommandRunPolicy::QueueUntilIdle,
            },
            SlashCommandInfo {
                name: "review".to_string(),
                description: "Review uncommitted changes".to_string(),
                argument_hint: None,
                source: SlashCommandSource::Adapter,
                arguments: SlashCommandArguments::None,
                run_policy: SlashCommandRunPolicy::QueueUntilIdle,
            },
            SlashCommandInfo {
                name: "skills".to_string(),
                description: "Choose an installed Codex skill".to_string(),
                argument_hint: Some("<skill>".to_string()),
                source: SlashCommandSource::Adapter,
                arguments: SlashCommandArguments::Skills,
                run_policy: SlashCommandRunPolicy::Immediate,
            },
            SlashCommandInfo {
                name: "fork".to_string(),
                description: "Branch this conversation in front of an earlier prompt".to_string(),
                argument_hint: None,
                source: SlashCommandSource::Adapter,
                arguments: SlashCommandArguments::None,
                // A branch is anchored on a turn the server has finished, so
                // one asked for mid-turn could not name the turn in progress.
                run_policy: SlashCommandRunPolicy::IdleOnly,
            },
            SlashCommandInfo {
                name: "goal".into(),
                description: "Set, inspect, pause, resume, or clear a persistent goal".into(),
                argument_hint: Some("[objective|pause|resume|clear]".into()),
                source: SlashCommandSource::Adapter,
                arguments: SlashCommandArguments::Freeform,
                run_policy: SlashCommandRunPolicy::Immediate,
            },
            SlashCommandInfo {
                name: "side".into(),
                description: "Open a side chat forked from this conversation".into(),
                argument_hint: Some("[question]".into()),
                source: SlashCommandSource::Adapter,
                arguments: SlashCommandArguments::Freeform,
                run_policy: SlashCommandRunPolicy::Immediate,
            },
        ]
    }

    fn request_goal(&mut self) {
        let Some(thread_id) = self.conversation.thread_id.clone() else {
            return;
        };

        self.send_query(
            QueryKind::Goal(self.conversation.goal_revision),
            json!({
                "method": "thread/goal/get", "params": {"threadId": thread_id}
            }),
        );
    }

    /// Attach a conversation to the shared app-server, starting and
    /// initializing the host only when no compatible generation is live.
    /// Messages for this conversation are handed to `deliver` from the host's
    /// reader task, so callers hop threads before invoking [`Session::process`].
    pub async fn spawn(
        launch: &LaunchConfig,
        host_catalog: &[LaunchConfig],
        workspace: &AgentWorkspace,
        deliver: impl Fn(Value) + Send + Sync + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        Self::spawn_inner(
            launch,
            host_catalog,
            workspace,
            ConversationStart::default(),
            deliver,
            on_stderr,
        )
        .await
    }

    /// Attach directly to an existing thread without creating a disposable
    /// empty thread first. Replay can be suppressed when the caller already
    /// retains the visible transcript in place.
    pub async fn spawn_resuming(
        launch: &LaunchConfig,
        host_catalog: &[LaunchConfig],
        workspace: &AgentWorkspace,
        thread_id: String,
        suppress_replay: bool,
        deliver: impl Fn(Value) + Send + Sync + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        Self::spawn_inner(
            launch,
            host_catalog,
            workspace,
            ConversationStart {
                resume: Some(thread_id),
                suppress_replay,
                team: None,
                side: None,
            },
            deliver,
            on_stderr,
        )
        .await
    }

    /// Open a side conversation: an ephemeral fork of `side`'s parent
    /// thread under its own registration on the shared host. The parent
    /// keeps its thread, routes, and running turn; the fork inherits its
    /// model context up to the moment the server takes the snapshot.
    pub async fn spawn_side(
        launch: &LaunchConfig,
        host_catalog: &[LaunchConfig],
        workspace: &AgentWorkspace,
        side: SideStart,
        deliver: impl Fn(Value) + Send + Sync + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        Self::spawn_inner(
            launch,
            host_catalog,
            workspace,
            ConversationStart {
                side: Some(side),
                ..ConversationStart::default()
            },
            deliver,
            on_stderr,
        )
        .await
    }

    async fn spawn_inner(
        launch: &LaunchConfig,
        host_catalog: &[LaunchConfig],
        workspace: &AgentWorkspace,
        start: ConversationStart,
        deliver: impl Fn(Value) + Send + Sync + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        let thread_profile: ThreadProfile = launch.into();
        let host = CodexHost::acquire(launch, host_catalog, on_stderr).await?;
        let deliver: SessionDelivery = Arc::new(deliver);
        let root_delivery = Arc::clone(&deliver);
        let registration_id = host.register(move |message| root_delivery(message));

        let mut session = Self {
            host: Some(host),
            conversation: ThreadState::default(),
            registration_id,
            deliver,
            detached: false,
            control: ControlState::default(),
            next_title_generation_id: 0,
            title_generation: None,
            history_cursor: None,
            history_scope: SessionScope::default(),
            skill_refresh: SkillRefreshState::default(),
            thread_profile,
            workspace: workspace.clone(),
            initial_resume: start.resume,
            suppress_resume_replay: start.suppress_replay,
            background: CodexTasks::default(),
            team: start.team.map(TeamState::new),
            side: start.side.map(Box::new),
            side_ready: None,
        };

        session.request_skills(false);

        session.start_initial_thread();

        Ok(session)
    }

    pub fn thread_id(&self) -> Option<&str> {
        self.conversation.thread_id.as_deref()
    }

    pub fn has_active_operation(&self) -> bool {
        self.conversation.current_turn.is_some()
            || self.conversation.has_pending_approval()
            || self.conversation.questions.has_active_request()
            || self.control.has_command()
            || self.conversation.compaction.active.is_some()
    }

    /// Detach from the shared host now; the returned future waits for the
    /// host process only when this session was its last owner.
    pub fn shutdown(
        &mut self,
        timeout: Duration,
        force: bool,
    ) -> BoxFuture<'static, Result<(), String>> {
        if self.detached {
            return ready(Ok(())).boxed();
        }

        self.cancel_title_generation();

        if let (Some(thread_id), Some(turn_id)) = (
            self.conversation.thread_id.clone(),
            self.conversation.current_turn.clone(),
        ) {
            let rpc_id = self.alloc_rpc_id();

            self.send(turn_interrupt_request(rpc_id, &thread_id, &turn_id));
        }

        if let Some(thread_id) = self.conversation.thread_id.clone() {
            let rpc_id = self.alloc_rpc_id();

            self.send(json!({
                "jsonrpc": "2.0",
                "id": rpc_id,
                "method": "thread/unsubscribe",
                "params": {"threadId": thread_id},
            }));
        }

        let stopping = match self.host.take() {
            Some(host) if host.detach(self.registration_id) => {
                host.shutdown(timeout, force).boxed()
            }
            _ => ready(Ok(())).boxed(),
        };

        self.control.close();

        self.detached = true;

        stopping
    }

    fn sync_descendant_owners(&self) {
        if let Some(host) = &self.host {
            host.claim_descendants(self.registration_id, self.background.confirmed_thread_ids());
        }
    }

    /// Handle one message from the server: advances the handshake, answers
    /// protocol-level requests, and returns the events a chat UI reacts to.
    pub fn process(&mut self, message: Value) -> Vec<Event> {
        if self.detached || self.control.is_closed() {
            return Vec::new();
        }

        let id = message["id"].as_u64();
        let method = message["method"].as_str().map(str::to_owned);

        if method.as_deref() == Some(TITLE_GENERATION_RESULT_METHOD) {
            return self.apply_title_generation_result(&message["params"]);
        }

        let mut events = match (id, method.as_deref()) {
            (Some(rpc_id), Some(method)) => self.on_server_request(rpc_id, method, &message),
            (Some(rpc_id), None) => self.on_response(rpc_id, &message),
            (None, Some(method)) => self.on_notification(method, &message["params"]),
            (None, None) => Vec::new(),
        };

        // An answer frees the approval surface outside this call; the next
        // waiting approval shows with whatever the server sends next.
        events.extend(self.conversation.next_approval());

        self.sync_descendant_owners();

        events
    }

    /// Send text plus the exact skill identity selected by a client picker.
    /// Text-only callers keep the original one-item request shape.
    /// Send text plus the exact skill identity selected by a client picker,
    /// and the local images the message carries. The server reads each image
    /// from the path given, so the caller keeps the file readable until the
    /// request has been written.
    pub(crate) fn send_user_message_with_skill(
        &mut self,
        text: &str,
        settings: &ThreadSettings,
        skill: Option<&SkillReference>,
        images: &[PathBuf],
    ) -> SendOutcome {
        if let Some(team) = &self.team
            && (!team.can_send() || self.conversation.current_turn.is_some())
        {
            return SendOutcome::NotReady;
        }

        let Some(thread_id) = self.conversation.thread_id.clone() else {
            return SendOutcome::NotReady;
        };

        let rpc_id = self.alloc_rpc_id();
        let input = codex_user_input(text, skill, images);
        let params = turn_start_params(&thread_id, input, settings, &self.workspace);

        if let Some(turn_id) = self.conversation.current_turn.clone() {
            if let Err(message) = self.try_send(json!({
                "jsonrpc": "2.0",
                "id": rpc_id,
                "method": "turn/steer",
                "params": {
                    "threadId": thread_id,
                    "expectedTurnId": turn_id,
                    "input": params["input"],
                },
            })) {
                return SendOutcome::Rejected { message };
            }

            self.control.track(
                rpc_id,
                ControlOperation::Steer {
                    next_turn_params: params,
                },
            );

            return SendOutcome::Steered;
        }

        if let Err(message) = self.try_send(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "turn/start",
            "params": params,
        })) {
            return SendOutcome::Rejected { message };
        }

        SendOutcome::StartedTurn
    }

    /// Submit the first primary prompt and start its isolated title request
    /// only after the primary thread accepts the prompt. The title is
    /// generated from the request's description rather than from `text`:
    /// the two differ when the prompt carries instructions around what the
    /// user asked, and the title should name what the user asked.
    pub(crate) fn send_user_message_with_generated_title(
        &mut self,
        text: &str,
        settings: &ThreadSettings,
        skill: Option<&SkillReference>,
        images: &[PathBuf],
        title: &ConversationTitleRequest,
    ) -> SendOutcome {
        let outcome = self.send_user_message_with_skill(text, settings, skill, images);

        if matches!(outcome, SendOutcome::StartedTurn | SendOutcome::Steered) {
            self.begin_title_generation(&title.description, &title.provisional_title);
        }

        outcome
    }

    /// Execute Codex operations that map directly to dedicated app-server
    /// requests. `/skills` is handled entirely by the UI picker and never
    /// reaches this method.
    pub fn execute_slash_command(&mut self, name: &str, arguments: &str) -> SlashCommandOutcome {
        let Some(thread_id) = self.conversation.thread_id.clone() else {
            return SlashCommandOutcome::NotReady;
        };

        let Some(command) = CodexCommand::parse(name) else {
            return SlashCommandOutcome::Rejected {
                message: format!("Unsupported Codex command: /{name}"),
            };
        };

        // A goal is read and set alongside a running turn, and it is the only
        // command that takes an argument.
        if command != CodexCommand::Goal {
            if self.conversation.current_turn.is_some() {
                return SlashCommandOutcome::Rejected {
                    message: "Codex is already running a turn.".to_string(),
                };
            }

            if !arguments.trim().is_empty() {
                return SlashCommandOutcome::Rejected {
                    message: format!("/{name} does not accept arguments."),
                };
            }
        }

        let rpc_id = self.alloc_rpc_id();

        if let Err(message) = self.try_send(command.request(rpc_id, &thread_id, arguments)) {
            return SlashCommandOutcome::Rejected { message };
        }

        if command == CodexCommand::Compact {
            self.conversation.compaction.request_manual();
        }

        self.control
            .track(rpc_id, ControlOperation::Command(command));

        SlashCommandOutcome::Accepted
    }

    /// Interrupt the running turn (the Esc/Ctrl-C equivalent).
    pub fn interrupt(&mut self) -> bool {
        let (Some(thread_id), Some(turn_id)) = (
            self.conversation.thread_id.clone(),
            self.conversation.current_turn.clone(),
        ) else {
            return false;
        };

        let rpc_id = self.alloc_rpc_id();

        if self
            .try_send(turn_interrupt_request(rpc_id, &thread_id, &turn_id))
            .is_err()
        {
            return false;
        }

        // An explicit stop must not be undone by a late steering refusal.
        self.control.cancel_steering_retries();

        true
    }

    /// Switch this session onto a persisted thread. The response carries the
    /// reconstructed turn history (emitted as [`Event::Replay`]) and the
    /// thread's persisted settings (emitted as [`Event::Ready`]); subsequent
    /// `turn/start` calls append to the resumed thread. On failure the
    /// session keeps the thread it started with, so the tab stays usable.
    pub fn resume_thread(&mut self, thread_id: &str) -> bool {
        let params = thread_resume_params(thread_id, &self.thread_profile);

        if self
            .try_send_query(
                QueryKind::Resume,
                json!({
                    "jsonrpc": "2.0",
                    "method": "thread/resume",
                    "params": params,
                }),
            )
            .is_err()
        {
            return false;
        }

        self.cancel_title_generation();

        true
    }

    /// Ask which prompts this conversation can be branched in front of.
    ///
    /// The thread's own history answers it, so the list covers turns from
    /// before this session resumed the thread as well as the ones it watched
    /// run. Reading it per request rather than accumulating it as turns go by
    /// also keeps the offer honest after a compaction rewrites the thread.
    pub fn request_fork_checkpoints(&mut self) -> bool {
        let Some(thread_id) = self.conversation.thread_id.clone() else {
            return false;
        };

        self.try_send_query(
            QueryKind::Checkpoints,
            json!({
                "jsonrpc": "2.0",
                "method": "thread/read",
                "params": {"threadId": thread_id, "includeTurns": true},
            }),
        )
        .is_ok()
    }

    /// Branch the thread at `anchor` and move this session onto the copy.
    ///
    /// The reply carries the same reconstructed history and persisted settings
    /// `thread/resume` answers with, so it is read by the same handler and the
    /// tab lands in the branch exactly as it lands in a resumed conversation.
    /// The source thread is left untouched.
    pub(crate) fn fork_thread(&mut self, anchor: &ForkAnchor) -> Result<(), String> {
        let ForkAnchor::CodexThrough(last_turn_id) = anchor else {
            return Err("that branch point belongs to another agent".to_string());
        };

        let Some(thread_id) = self.conversation.thread_id.clone() else {
            return Err("this conversation has no thread to branch".to_string());
        };

        let mut params = thread_resume_params(&thread_id, &self.thread_profile);

        params["lastTurnId"] = json!(last_turn_id);

        self.try_send_query(
            QueryKind::Fork,
            json!({
                "jsonrpc": "2.0",
                "method": "thread/fork",
                "params": params,
            }),
        )?;

        self.cancel_title_generation();

        Ok(())
    }

    /// Ask for the first page of session history over `scope`. Replaces
    /// whatever an earlier scope was paging through, so the caller drops the
    /// rows it already holds.
    pub fn request_history(&mut self, scope: SessionScope) {
        self.history_scope = scope;
        self.history_cursor = None;

        let params = thread_list_params(&self.thread_profile, None, scope, &self.workspace);

        self.send_query(
            QueryKind::History,
            json!({
                "jsonrpc": "2.0",
                "method": "thread/list",
                "params": params,
            }),
        );
    }

    /// Request the next history page; a no-op when the final page arrived.
    pub fn request_more_history(&mut self) {
        let Some(cursor) = self.history_cursor.take() else {
            return;
        };

        let params = thread_list_params(
            &self.thread_profile,
            Some(&cursor),
            self.history_scope,
            &self.workspace,
        );

        self.send_query(
            QueryKind::History,
            json!({
                "jsonrpc": "2.0",
                "method": "thread/list",
                "params": params,
            }),
        );
    }

    /// Reload descendant threads for the current parent. Opening the panel can
    /// ask for fresher data; a request already in flight is left to complete
    /// instead of racing a second pass over the same pages.
    pub fn refresh_background_tasks(&mut self) {
        self.start_descendant_discovery();
    }

    /// Stop one child agent, leaving the parent's turn running. Returns whether
    /// the request went out: a child whose active turn is not known cannot be
    /// named in `turn/interrupt`, and the caller reports that rather than
    /// pretending the child was stopped.
    pub fn interrupt_background_task(&mut self, thread_id: &str) -> bool {
        let rpc_id = self.alloc_rpc_id();

        let Some(request) = self.background.interrupt_request(rpc_id, thread_id) else {
            return false;
        };

        // The child's own `turn/completed` reports the interruption, so the row
        // moves to Interrupted through the same path as any other outcome.
        self.try_send(request).is_ok()
    }

    /// Read one descendant's stored conversation. A read already in flight for
    /// the same child is left to finish, and an unknown thread is ignored.
    pub fn load_background_task_transcript(&mut self, thread_id: &str) -> Vec<Event> {
        let rpc_id = self.alloc_rpc_id();

        let Some(request) = self.background.transcript_request(rpc_id, thread_id) else {
            return Vec::new();
        };

        self.send(request);

        vec![Event::BackgroundTaskTranscript {
            key: BackgroundTaskKey::codex(thread_id),
            update: BackgroundTaskTranscriptUpdate::state(BackgroundTaskLoadState::Loading),
        }]
    }

    fn start_descendant_discovery(&mut self) {
        let Some(thread_id) = self.conversation.thread_id.clone() else {
            return;
        };

        self.background.set_root(&thread_id);

        if self.background.query_in_flight() {
            return;
        }

        let rpc_id = self.alloc_rpc_id();

        if let Some(request) = self.background.descendant_request(rpc_id, None) {
            self.send(request);
        }
    }

    /// Publish the task snapshot only for a real change, so an unchanged
    /// repeat of a known state does not repaint the panel.
    fn background_events(&self, changed: bool) -> Vec<Event> {
        if !changed {
            return Vec::new();
        }

        self.background
            .snapshot()
            .map(Event::BackgroundTasks)
            .into_iter()
            .collect()
    }

    /// Answer the approval request on screen (`"accept"` / `"decline"`); a
    /// no-op when none is shown.
    pub fn respond_approval(&mut self, decision: &str) -> bool {
        let Some(rpc_id) = self.conversation.shown_approval() else {
            return false;
        };

        if self
            .try_send(json!({
                "jsonrpc": "2.0",
                "id": rpc_id,
                "result": {"decision": decision},
            }))
            .is_err()
        {
            return false;
        }

        self.conversation.answered_approval(rpc_id);

        true
    }

    fn request_models(&mut self) {
        self.send_query(
            QueryKind::Models,
            json!({
                "jsonrpc": "2.0",
                "method": "model/list",
                "params": {"limit": 100},
            }),
        );
    }

    fn alloc_rpc_id(&mut self) -> u64 {
        self.control.alloc_id()
    }

    fn request_skills(&mut self, force_reload: bool) {
        if self.skill_refresh.queue_if_in_flight(force_reload) {
            return;
        }

        let rpc_id = self.alloc_rpc_id();

        self.skill_refresh.start(rpc_id);

        let request = skills_list_request(rpc_id, force_reload, &self.workspace);

        self.send(request);
    }

    /// Interactive submissions need the write result to retain a rejected draft.
    fn try_send(&mut self, message: Value) -> Result<(), String> {
        if self.detached || self.control.is_closed() {
            return Err("Codex app-server is not connected".to_string());
        }

        let outgoing = ControlState::outgoing(&message);

        self.host
            .as_ref()
            .ok_or("Codex app-server is not connected")?
            .send(self.registration_id, message)?;

        if let Some((id, operation)) = outgoing {
            self.control.track(id, operation);
        }

        Ok(())
    }

    fn retain_request_routes(&self) {
        if let Some(host) = &self.host {
            host.retain_requests(self.registration_id, &self.control.request_ids());
        }
    }

    fn try_send_query(&mut self, kind: QueryKind, mut message: Value) -> Result<(), String> {
        let id = self.alloc_rpc_id();

        message["id"] = json!(id);

        self.try_send(message)?;

        self.control.track_query(id, kind);

        self.retain_request_routes();

        Ok(())
    }

    fn send_query(&mut self, kind: QueryKind, mut message: Value) {
        let id = self.alloc_rpc_id();

        message["id"] = json!(id);

        self.send(message);

        self.control.track_query(id, kind);

        self.retain_request_routes();
    }

    fn send(&mut self, message: Value) {
        let outgoing = ControlState::outgoing(&message);

        let request_id = message["method"]
            .is_string()
            .then(|| message["id"].as_u64())
            .flatten();

        if let Err(error) = self.try_send(message) {
            if let Some((id, operation)) = outgoing {
                self.control.track(id, operation);
            }

            tracing::warn!("could not write Codex app-server request: {error}");

            // Background requests already have local pending state. Deliver the
            // rejection through the usual response path so it can settle that
            // state even though the shared host remains available.
            if let Some(id) = request_id {
                (self.deliver)(json!({"id": id, "error": {"message": error}}));
            }
        }
    }

    fn on_server_request(&mut self, rpc_id: u64, method: &str, message: &Value) -> Vec<Event> {
        match method {
            "item/tool/call" => self.on_team_decision(rpc_id, &message["params"]),
            "item/tool/requestUserInput" => on_question_request(self, rpc_id, &message["params"]),
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                let params = &message["params"];

                let description = if method == "item/commandExecution/requestApproval" {
                    format!("Run command: `{}`", stringify_command(&params["command"]))
                } else {
                    format!(
                        "Apply file changes: {}",
                        file_change_paths(&params["changes"])
                    )
                };

                self.conversation
                    .request_approval(rpc_id, description)
                    .into_iter()
                    .collect()
            }
            // Any other server→client request is unsupported by this client;
            // an error reply keeps the turn from hanging (the same strategy
            // `codex exec` uses for approvals).
            _ => {
                self.send(json!({
                    "jsonrpc": "2.0",
                    "id": rpc_id,
                    "error": {"code": -32601, "message": "not supported by NiumaTerm agent tab"},
                }));

                Vec::new()
            }
        }
    }

    fn on_response(&mut self, rpc_id: u64, message: &Value) -> Vec<Event> {
        let (pending_command, query) = match self.control.finish(rpc_id) {
            Some(ControlOperation::Steer { next_turn_params })
                if message["error"]["message"] == "no active turn to steer" =>
            {
                // The server rejected this input before admission. Starting it
                // again is safe only for this explicit refusal, never for a
                // timeout whose delivery outcome is unknown.
                let id = self.alloc_rpc_id();

                return match self.try_send(json!({
                    "jsonrpc": "2.0", "id": id, "method": "turn/start",
                    "params": next_turn_params,
                })) {
                    Ok(()) => Vec::new(),
                    Err(message) => vec![Event::Error {
                        message,
                        fatal: false,
                    }],
                };
            }
            Some(ControlOperation::Command(command)) => (Some(command), None),
            Some(ControlOperation::Query(kind)) => (None, Some(kind)),
            Some(
                ControlOperation::Other
                | ControlOperation::ThreadRequest
                | ControlOperation::Steer { .. },
            ) => (None, None),
            Some(ControlOperation::ThreadName)
                if message["error"]["data"]["requestTimedOut"].as_bool() == Some(true) =>
            {
                (None, None)
            }
            Some(ControlOperation::ThreadName) | None => return Vec::new(),
        };

        if let Some(events) = on_question_response(self, rpc_id, message) {
            return events;
        }

        if self.skill_refresh.in_flight == Some(rpc_id) {
            let catalog = skill_catalog_from_response(message);
            let force_reload_again = self.skill_refresh.finish(rpc_id).unwrap_or(false);

            if force_reload_again {
                self.request_skills(true);
            }

            return vec![Event::Skills(catalog)];
        }

        // A descendant read answers before the shared error path so a failed
        // read reports an unavailable conversation instead of a session error.
        if self.background.is_transcript_read(rpc_id) {
            return self.on_transcript_read_response(rpc_id, message);
        }

        // Descendant discovery answers before the shared error path so a
        // failed page reports unavailable status instead of a session error.
        if self.background.is_query(rpc_id) {
            return self.on_descendant_page(rpc_id, message);
        }

        if let Some(error) = message["error"]["message"].as_str() {
            if matches!(query, Some(QueryKind::Goal(_))) {
                return Vec::new();
            }

            return self.on_response_error(pending_command, query, error);
        }

        if let Some(command) = pending_command {
            if command == CodexCommand::Goal {
                // A live update can arrive before the command reply. Read the
                // current goal with revision protection instead of restoring
                // the older state captured in that reply.
                self.request_goal();

                return vec![Event::SlashCommandResult {
                    name: command.name().to_string(),
                    outcome: SlashCommandOutcome::Completed {
                        message: None,
                        approval: None,
                    },
                }];
            }

            // Dedicated command requests acknowledge scheduling before their
            // turn and item notifications report the actual work. Treating
            // this response as completion could admit another queued command
            // while the thread is busy.
            return vec![Event::SlashCommandResult {
                name: command.name().to_string(),
                outcome: SlashCommandOutcome::Accepted,
            }];
        }

        match query {
            Some(QueryKind::Goal(revision)) => {
                if revision == self.conversation.goal_revision {
                    vec![Event::GoalUpdated(goal_status(&message["result"]["goal"]))]
                } else {
                    Vec::new()
                }
            }
            Some(QueryKind::Start) => {
                let result = &message["result"];

                self.conversation.thread_id = result["thread"]["id"].as_str().map(str::to_owned);

                self.request_goal();

                self.request_models();

                // History for the empty-tab session list, over whatever scope
                // the tab last asked for.
                self.request_history(self.history_scope);

                self.start_descendant_discovery();

                self.finish_team_start(vec![Event::Ready(parse_thread_settings(result))])
            }
            // A side conversation is ready only once its boundary is in the
            // model history, so the fork reply writes it and waits. It lists
            // no history and discovers no descendants: it is kept out of the
            // history list and may not use sub-agents.
            Some(QueryKind::SideFork) => {
                let result = &message["result"];

                let Some(thread_id) = result["thread"]["id"].as_str().map(str::to_owned) else {
                    return vec![Event::Error {
                        message: "Could not start the side chat: the fork named no thread.".into(),
                        fatal: true,
                    }];
                };

                self.conversation.thread_id = Some(thread_id.clone());

                self.side_ready = Some(Box::new(parse_thread_settings(result)));

                self.send_query(QueryKind::SideBoundary, side_boundary_request(&thread_id));

                Vec::new()
            }
            Some(QueryKind::SideBoundary) => {
                self.request_goal();

                self.request_models();

                let boundary = self.thread_id().map(side_boundary_item);

                let ready = Event::Ready(
                    self.side_ready
                        .take()
                        .map(|settings| *settings)
                        .unwrap_or_default(),
                );

                // The boundary lands after Ready so it opens the side
                // transcript, above the first question.
                [ready]
                    .into_iter()
                    .chain(boundary.map(Event::ItemStarted))
                    .collect()
            }
            Some(QueryKind::Models) => {
                let models = if self.thread_profile.provider.is_some() {
                    parse_models(&json!({"data": []}), self.thread_profile.model.as_deref())
                } else {
                    parse_models(&message["result"], self.thread_profile.model.as_deref())
                };

                vec![Event::Models(models)]
            }
            Some(QueryKind::History) => {
                let result = &message["result"];

                self.history_cursor = result["nextCursor"].as_str().map(str::to_owned);

                // The thread this session just started is part of the request
                // listing but is the tab's own live (empty) thread, and a
                // history row for it would resume a conversation the user is
                // already in.
                vec![Event::History(parse_thread_summaries(
                    result,
                    self.conversation.thread_id.as_deref(),
                ))]
            }
            Some(QueryKind::Checkpoints) => vec![Event::ForkCheckpoints(Ok(
                parse_fork_checkpoints(&message["result"]["thread"]["turns"]),
            ))],
            // A branch answers with the same payload a resume answers with,
            // down to the settings block, so both switch this session onto the
            // thread the reply names.
            Some(QueryKind::Resume | QueryKind::Fork) => {
                self.on_thread_switched(&message["result"])
            }
            _ => Vec::new(),
        }
    }

    fn on_transcript_read_response(&mut self, rpc_id: u64, message: &Value) -> Vec<Event> {
        let Some(thread_id) = self.background.finish_transcript_read(rpc_id) else {
            return Vec::new();
        };

        let key = BackgroundTaskKey::codex(&thread_id);

        let update = match message["error"]["message"].as_str() {
            Some(error) => {
                BackgroundTaskTranscriptUpdate::state(BackgroundTaskLoadState::Unavailable {
                    message: error.to_owned(),
                })
            }
            // The same parser the parent transcript uses, so a child's
            // tool cards cannot lose output or status relative to it. A
            // child's conversation is presented as one stream, so its turn
            // grouping is flattened away.
            None => BackgroundTaskTranscriptUpdate::loaded(
                self.background.with_launch_message(
                    &thread_id,
                    parse_replay(&message["result"]["thread"]["turns"])
                        .into_iter()
                        .flat_map(|turn| turn.items)
                        .map(|entry| entry.item)
                        .collect(),
                ),
            ),
        };

        vec![Event::BackgroundTaskTranscript { key, update }]
    }

    fn on_descendant_page(&mut self, rpc_id: u64, message: &Value) -> Vec<Event> {
        if let Some(error) = message["error"]["message"].as_str() {
            let changed = self.background.fail_query(rpc_id, error);

            return self.background_events(changed);
        }

        let (mut changed, next_cursor) = self
            .background
            .apply_descendants(rpc_id, &message["result"]);

        if let Some(cursor) = next_cursor {
            let next_rpc_id = self.alloc_rpc_id();

            if let Some(request) = self
                .background
                .descendant_request(next_rpc_id, Some(&cursor))
            {
                self.send(request);

                changed = true;
            }
        }

        self.background_events(changed)
    }

    fn on_response_error(
        &mut self,
        pending_command: Option<CodexCommand>,
        query: Option<QueryKind>,
        error: &str,
    ) -> Vec<Event> {
        if let Some(command) = pending_command {
            if command == CodexCommand::Compact {
                self.conversation.compaction.reject_manual_request();
            }

            let name = command.name();

            return vec![Event::SlashCommandResult {
                name: name.to_string(),
                outcome: SlashCommandOutcome::Rejected {
                    message: format!("/{name} failed: {error}"),
                },
            }];
        }

        // A branch-point list nobody could read leaves the picker with
        // nothing to show, which is the picker's own failure to report
        // rather than something that happened to the conversation.
        if query == Some(QueryKind::Checkpoints) {
            return vec![Event::ForkCheckpoints(Err(error.to_string()))];
        }

        // A failed resume (deleted/corrupt thread) is not fatal: the
        // session still has the thread it started with, so the composer
        // keeps working for a fresh conversation.
        let initial_resume_failed =
            query == Some(QueryKind::Resume) && self.initial_resume.is_some();

        let message = match query {
            Some(QueryKind::Resume) => format!("Could not resume session: {error}"),
            // A refused branch leaves the session on the thread it was
            // already holding, so the conversation stays usable.
            Some(QueryKind::Fork) => format!("Could not branch this conversation: {error}"),
            Some(QueryKind::SideFork | QueryKind::SideBoundary) => {
                format!("Could not start the side chat: {error}")
            }
            _ => error.to_string(),
        };

        // A side conversation that failed to fork or to take its boundary
        // has nothing it could safely answer from.
        let fatal = initial_resume_failed
            || matches!(
                query,
                Some(QueryKind::Start | QueryKind::SideFork | QueryKind::SideBoundary)
            );

        vec![Event::Error { message, fatal }]
    }

    fn on_thread_switched(&mut self, result: &Value) -> Vec<Event> {
        self.control.reset_thread();

        self.retain_request_routes();

        self.conversation.end_thread();

        self.conversation.thread_id = result["thread"]["id"].as_str().map(str::to_owned);
        self.initial_resume = None;
        self.conversation.plan_revision += 1;
        self.conversation.goal_revision += 1;

        self.request_goal();

        if let Some(path) = result["thread"]["path"].as_str() {
            spawn_plan_restore(
                PathBuf::from(path),
                self.conversation.thread_id.clone(),
                self.conversation.plan_revision,
                self.deliver.clone(),
            );
        }

        // A resumed parent can already have finished descendants, and
        // a reconnect resumes into a new process with none of the live
        // child state the previous one observed.
        self.start_descendant_discovery();

        self.retain_team_history(&result["thread"]["turns"]);

        let events = resumed_thread_events(result, take(&mut self.suppress_resume_replay));

        self.finish_team_start(events)
    }

    fn on_notification(&mut self, method: &str, params: &Value) -> Vec<Event> {
        if method == HOST_EXIT_METHOD {
            return self.on_host_exit(params);
        }

        if method == "thread/compacted" {
            // Current servers can publish this deprecated notification beside
            // the authoritative item lifecycle. Ignoring it prevents a second
            // boundary for the same context rewrite.
            return Vec::new();
        }

        if method == "rawResponseItem/completed" {
            if let Some(thread_id) = notification_thread_id(params) {
                self.background
                    .observe_raw_response_item(thread_id, &params["item"]);
            }

            return Vec::new();
        }

        // Thread routing happens before any parent state change: a descendant's
        // turn completion must not clear a still-running parent turn, and an
        // unrelated thread's content must not enter the parent transcript.
        if THREAD_SCOPED_NOTIFICATIONS.contains(&method) {
            let thread_id = notification_thread_id(params).map(str::to_owned);

            match self.background.scope(thread_id.as_deref()) {
                ThreadScope::Descendant => {
                    let thread_id = thread_id.unwrap_or_default();

                    let changed = self
                        .background
                        .observe_descendant_notification(&thread_id, method, params);

                    return self.background_events(changed);
                }
                ThreadScope::Unrelated => {
                    let thread_id = thread_id.unwrap_or_default();

                    self.background
                        .hold_unrelated_notification(&thread_id, method, params);

                    return Vec::new();
                }
                // A thread-scoped notification that carries no usable thread id
                // keeps parent handling: the parent's running state is the only
                // conversation this session can be describing.
                ThreadScope::Parent | ThreadScope::Unscoped => {}
            }
        }

        if method == "skills/changed" {
            self.request_skills(true);

            return Vec::new();
        }

        // Child discovery observes only parent items after thread routing.
        // Its panel updates follow the parent's transcript events.
        let children_changed = matches!(method, "item/started" | "item/completed")
            && params["item"]["type"].as_str() != Some("contextCompaction")
            && self.background.observe_parent_item(&params["item"]);

        let mut events = self.conversation.on_notification(method, params);

        events.extend(self.background_events(children_changed));

        events
    }

    fn on_host_exit(&mut self, params: &Value) -> Vec<Event> {
        self.cancel_title_generation();

        self.conversation.end_thread();

        self.control.close();

        self.skill_refresh = SkillRefreshState::default();

        vec![Event::HostExited {
            message: params["message"]
                .as_str()
                .unwrap_or("Codex app-server stopped unexpectedly")
                .to_string(),
        }]
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        nmt_platform::runtime().spawn(self.shutdown(DROP_SHUTDOWN_GRACE, true));
    }
}

/// Serialized values for approval-policy selection (`AskForApproval` serializes
/// kebab-case).
pub const APPROVAL_OPTIONS: [&str; 3] = ["untrusted", "on-request", "never"];

/// Serialized values for choosing who handles eligible approval requests.
pub const APPROVAL_REVIEWER_OPTIONS: [&str; 2] = ["user", "auto_review"];

/// `(serialized value, display label)` for sandbox selection (`SandboxPolicy` uses a
/// camelCase `type` tag).
pub const SANDBOX_OPTIONS: [(&str, &str); 3] = [
    ("readOnly", "read-only"),
    ("workspaceWrite", "workspace-write"),
    ("dangerFullAccess", "full-access"),
];

impl Session {
    /// A user-authored name invalidates any generated replacement before the
    /// provider write is queued, so a late worker result cannot rename it.
    pub(crate) fn rename_thread(&mut self, name: &str) -> bool {
        self.cancel_title_generation();

        let Some(thread_id) = self.conversation.thread_id.clone() else {
            return false;
        };

        let name = name.trim();

        if name.is_empty() {
            return false;
        }

        let rpc_id = self.alloc_rpc_id();

        self.try_send(thread_name_request(rpc_id, &thread_id, name))
            .is_ok()
    }

    pub(crate) fn cancel_title_generation(&mut self) {
        if let Some(generation) = self.title_generation.take() {
            generation.cancel();
        }
    }

    pub(super) fn begin_title_generation(&mut self, prompt: &str, provisional_title: &str) {
        self.cancel_title_generation();

        let (Some(host), Some(root_thread_id)) =
            (self.host.as_ref(), self.conversation.thread_id.clone())
        else {
            self.queue_thread_name(provisional_title);

            return;
        };

        self.next_title_generation_id = self.next_title_generation_id.wrapping_add(1).max(1);

        let generation_id = self.next_title_generation_id;

        self.title_generation = Some(start_title_generation(
            Arc::clone(host),
            Arc::clone(&self.deliver),
            TitleGenerationRequest {
                generation_id,
                root_thread_id,
                provisional_title: provisional_title.to_string(),
                prompt: prompt.to_string(),
                profile: self.thread_profile.clone(),
                workspace: self.workspace.clone(),
            },
        ));
    }

    pub(super) fn apply_title_generation_result(&mut self, params: &Value) -> Vec<Event> {
        let Some(result) = parse_title_generation_result(params) else {
            return Vec::new();
        };

        let matches_active = self
            .title_generation
            .as_ref()
            .is_some_and(|active| active.accepts(&result, self.conversation.thread_id.as_deref()));

        if !matches_active {
            return Vec::new();
        }

        self.title_generation.take();

        let title = result.resolved_title().to_string();

        self.queue_thread_name(&title);

        vec![Event::TitleUpdated(title)]
    }

    fn queue_thread_name(&mut self, name: &str) {
        let Some(thread_id) = self.conversation.thread_id.clone() else {
            return;
        };

        // Keep later writes queued even while an earlier name is pending: a
        // user rename that follows a generated name must be the final request
        // the server applies.
        let rpc_id = self.alloc_rpc_id();

        self.send(thread_name_request(rpc_id, &thread_id, name));
    }
}

impl Session {
    pub fn team_recovered_turns(&self) -> &[RecoveredTeamTurn] {
        self.team
            .as_ref()
            .map_or(&[], |team| team.completed_turns.as_slice())
    }

    pub(super) fn retain_team_history(&mut self, turns: &Value) {
        let Some(team) = self.team.as_mut() else {
            return;
        };

        team.completed_turns = turns
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|turn| {
                if turn["status"].as_str() != Some("completed") {
                    return None;
                }

                let id = turn["id"].as_str().filter(|id| !id.is_empty())?;

                let text = turn["items"]
                    .as_array()?
                    .iter()
                    .rev()
                    .find(|item| item["type"].as_str() == Some("agentMessage"))
                    .and_then(|item| item["text"].as_str())
                    .unwrap_or_default();

                Some(RecoveredTeamTurn {
                    id: id.to_owned(),
                    text: text.to_owned(),
                })
            })
            .collect();
    }

    pub async fn spawn_team(
        launch: &LaunchConfig,
        host_catalog: &[LaunchConfig],
        workspace: &AgentWorkspace,
        resume: Option<String>,
        policy: TeamLaunch,
        deliver: impl Fn(Value) + Send + Sync + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        Self::spawn_inner(
            launch,
            host_catalog,
            workspace,
            ConversationStart {
                resume,
                suppress_replay: !policy.restore_transcript,
                team: Some(policy),
                side: None,
            },
            deliver,
            on_stderr,
        )
        .await
    }

    pub fn team_capabilities(&self, backend_generation: u64) -> ModeratorAdmission {
        let mut capabilities = ModeratorAdmission::unverified(AgentKind::Codex);

        if self
            .team
            .as_ref()
            .is_some_and(|team| team.ready && team.moderation_registered)
        {
            capabilities = ModeratorAdmission::CodexDynamicTools { backend_generation };
        }

        capabilities
    }

    pub(super) fn start_initial_thread(&mut self) {
        if let Some(side) = &self.side {
            let request = side_fork_request(side);

            self.send_query(QueryKind::SideFork, request);

            return;
        }

        let mut request = initial_thread_request(
            self.initial_resume.as_deref(),
            &self.thread_profile,
            &self.workspace,
        );

        if self.team.as_ref().is_some_and(|team| team.launch.moderator)
            && self.initial_resume.is_none()
        {
            request["params"]["dynamicTools"] = json!([decision_tool()]);
        }

        let kind = if self.initial_resume.is_some() {
            QueryKind::Resume
        } else {
            QueryKind::Start
        };

        self.send_query(kind, request);
    }

    pub(super) fn finish_team_start(&mut self, events: Vec<Event>) -> Vec<Event> {
        if let Some(team) = &mut self.team {
            team.moderation_registered = team.launch.moderator;
            team.ready = true;
        }

        events
    }

    pub(super) fn on_team_decision(&mut self, request_id: u64, params: &Value) -> Vec<Event> {
        let valid = params["tool"].as_str() == Some("team_decide")
            && params["threadId"].as_str() == self.thread_id()
            && params["turnId"].as_str() == self.conversation.current_turn.as_deref()
            && self.team.as_ref().is_some_and(|team| {
                team.ready
                    && team.moderation_registered
                    && !team.pending_decisions.contains_key(&request_id)
            });

        if !valid {
            self.send(json!({"id": request_id, "result": {"success": false, "contentItems": [{"type": "inputText", "text": "This discussion operation is unavailable for this session or turn."}]}}));

            return Vec::new();
        }

        let Some(turn) = params["turnId"].as_str() else {
            return Vec::new();
        };

        if let Some(team) = &mut self.team {
            team.pending_decisions.insert(request_id, turn.to_owned());
        }

        vec![Event::TeamDecision(TeamDecisionRequest {
            request_id,
            provider_turn: turn.to_owned(),
            arguments: params["arguments"].clone(),
        })]
    }

    pub fn respond_team_decision(
        &mut self,
        request: &TeamDecisionRequest,
        accepted: bool,
        explanation: &str,
    ) -> bool {
        let current = self
            .team
            .as_ref()
            .and_then(|team| team.pending_decisions.get(&request.request_id));

        if current != Some(&request.provider_turn)
            || self.conversation.current_turn.as_deref() != Some(request.provider_turn.as_str())
        {
            return false;
        }

        if self.try_send(json!({"id": request.request_id, "result": {"success": accepted, "contentItems": [{"type": "inputText", "text": explanation}]}})).is_err() {
            return false;
        }

        if let Some(team) = &mut self.team {
            team.pending_decisions.remove(&request.request_id);
        }

        true
    }
}

impl Session {
    pub(crate) fn restore_question_requests(&mut self, requests: Vec<QuestionRequest>) {
        restore_question_requests(self, requests);
    }

    /// Message dismissal settles locally; submitted answers resolve through
    /// later events.
    pub fn respond_input(
        &mut self,
        id: &str,
        answers: Option<Vec<Vec<String>>>,
        settings: &ThreadSettings,
    ) -> Result<QuestionResponse, String> {
        respond_input(self, id, answers, settings)
    }
}
