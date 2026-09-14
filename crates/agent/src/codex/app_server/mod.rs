//! Codex `app-server` chat session: process lifecycle, JSON-RPC handshake,
//! and translation of the backend protocol into typed events for a chat UI.
//!
//! The app-server protocol is Codex's supported integration surface for
//! third-party UIs (it powers the VS Code extension). Each `Session` owns one
//! conversation thread and shares its app-server host with other sessions.

pub use crate::background_task::{
    BackgroundTaskKey, BackgroundTaskTranscriptState, BackgroundTaskTranscriptUpdate,
};
pub use crate::chat::{
    Compaction, CompactionTrigger, ContextUsageScope, ContextWindowUsage, Event, ForkAnchor,
    ForkCheckpoint, Item, ModelInfo, ScopedTokenUsage, SendOutcome, SessionScope, SessionSummary,
    SkillCatalog, SkillInfo, SkillReference, SlashCommandArguments, SlashCommandInfo,
    SlashCommandOutcome, SlashCommandRunPolicy, SlashCommandSource, ThreadSettings,
    TokenUsageBreakdown,
};
pub use crate::codex::app_server::options::{
    APPROVAL_OPTIONS, APPROVAL_REVIEWER_OPTIONS, SANDBOX_OPTIONS,
};

pub(crate) use crate::codex::app_server::title_generation::provisional_title_from_prompt;

mod background_tasks;
mod compaction;
mod control;
mod conversation;
mod host;
mod options;
mod progress;
mod protocol;
mod questions;
mod skills;
mod team;
mod title_generation;

#[cfg(test)]
mod tests;

use std::mem::take;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
#[cfg(test)]
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

use crate::codex::app_server::background_tasks::{CodexTasks, ThreadScope, notification_thread_id};
use crate::codex::app_server::compaction::is_legacy_compaction_notification;
use crate::codex::app_server::control::{ControlOperation, ControlState, QueryKind};
use crate::codex::app_server::conversation::ThreadState;
#[cfg(test)]
use crate::codex::app_server::conversation::TurnOutputUsage;
use crate::codex::app_server::host::{CodexHost, HOST_EXIT_METHOD, RegistrationId};
use crate::codex::app_server::progress::{PLAN_RESTORED, goal_request, goal_status, read_plan};
#[cfg(test)]
use crate::codex::app_server::protocol::thread_start_params;
use crate::codex::app_server::protocol::{
    codex_command_request, codex_command_response, codex_user_input, file_change_paths,
    parse_fork_checkpoints, parse_models, parse_replay, parse_thread_settings,
    parse_thread_summaries, resumed_thread_events, skills_list_request, stringify_command,
    thread_list_params, thread_resume_params, turn_start_params,
};
use crate::codex::app_server::questions::QuestionState;
#[cfg(test)]
use crate::codex::app_server::skills::parse_skill_catalog;
use crate::codex::app_server::skills::{SkillRefreshState, skill_catalog_from_response};
use crate::codex::app_server::team::TeamState;
use crate::codex::app_server::title_generation::{
    TITLE_GENERATION_RESULT_METHOD, TitleGenerationHandle,
};
use crate::session::team_capabilities::TeamLaunch;
use crate::workspace::AgentWorkspace;
use crate::{CodexProviderConfig, LaunchConfig};

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
    provider: Option<CodexProviderConfig>,
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
}

#[derive(Default)]
struct ConversationStart {
    resume: Option<String>,
    suppress_replay: bool,
    team: Option<TeamLaunch>,
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
    /// reader thread, so callers hop threads before invoking [`Session::process`].
    pub fn spawn(
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
    }

    /// Attach directly to an existing thread without creating a disposable
    /// empty thread first. Replay can be suppressed when the caller already
    /// retains the visible transcript in place.
    pub fn spawn_resuming(
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
            },
            deliver,
            on_stderr,
        )
    }

    fn spawn_inner(
        launch: &LaunchConfig,
        host_catalog: &[LaunchConfig],
        workspace: &AgentWorkspace,
        start: ConversationStart,
        deliver: impl Fn(Value) + Send + Sync + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        let thread_profile: ThreadProfile = launch.into();
        let host = CodexHost::acquire(launch, host_catalog, on_stderr)?;
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
            || self.conversation.pending_approval.is_some()
            || self.conversation.questions.has_active_request()
            || self.control.has_command()
            || self.conversation.compaction.active.is_some()
    }

    pub fn shutdown(&mut self, timeout: Duration, force: bool) -> Result<(), String> {
        if self.detached {
            return Ok(());
        }

        self.cancel_title_generation();

        if let (Some(thread_id), Some(turn_id)) = (
            self.conversation.thread_id.clone(),
            self.conversation.current_turn.clone(),
        ) {
            let rpc_id = self.alloc_rpc_id();

            self.send(json!({
                "jsonrpc": "2.0",
                "id": rpc_id,
                "method": "turn/interrupt",
                "params": {"threadId": thread_id, "turnId": turn_id},
            }));
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

        let result = if let Some(host) = self.host.take() {
            if host.detach(self.registration_id) {
                host.shutdown(timeout, force)
            } else {
                Ok(())
            }
        } else {
            Ok(())
        };

        self.control.close();

        self.detached = true;

        result
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

        let events = match (id, method.as_deref()) {
            (Some(rpc_id), Some(method)) => self.on_server_request(rpc_id, method, &message),
            (Some(rpc_id), None) => self.on_response(rpc_id, &message),
            (None, Some(method)) => self.on_notification(method, &message["params"]),
            (None, None) => Vec::new(),
        };

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

        if let Some(turn_id) = self.conversation.current_turn.clone() {
            if let Err(message) = self.try_send(json!({
                "jsonrpc": "2.0",
                "id": rpc_id,
                "method": "turn/steer",
                "params": {
                    "threadId": thread_id,
                    "expectedTurnId": turn_id,
                    "input": input,
                },
            })) {
                return SendOutcome::Rejected { message };
            }

            return SendOutcome::Steered;
        }

        let params = turn_start_params(&thread_id, input, settings, &self.workspace);

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
    /// only after the primary thread accepts the prompt.
    pub(crate) fn send_user_message_with_generated_title(
        &mut self,
        text: &str,
        settings: &ThreadSettings,
        skill: Option<&SkillReference>,
        images: &[PathBuf],
        provisional_title: &str,
    ) -> SendOutcome {
        let outcome = self.send_user_message_with_skill(text, settings, skill, images);

        if matches!(outcome, SendOutcome::StartedTurn | SendOutcome::Steered) {
            self.begin_title_generation(text, provisional_title);
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

        if name != "goal" && self.conversation.current_turn.is_some() {
            return SlashCommandOutcome::Rejected {
                message: "Codex is already running a turn.".to_string(),
            };
        }

        if name != "goal" && !arguments.trim().is_empty() {
            return SlashCommandOutcome::Rejected {
                message: format!("/{name} does not accept arguments."),
            };
        }

        let rpc_id = self.alloc_rpc_id();

        let request = if name == "goal" {
            Some(goal_request(rpc_id, &thread_id, arguments))
        } else {
            codex_command_request(rpc_id, &thread_id, name)
        };

        let Some(request) = request else {
            return SlashCommandOutcome::Rejected {
                message: format!("Unsupported Codex command: /{name}"),
            };
        };

        if let Err(message) = self.try_send(request) {
            return SlashCommandOutcome::Rejected { message };
        }

        if name == "compact" {
            self.conversation.compaction.request_manual();
        }

        self.control
            .track(rpc_id, ControlOperation::Command(name.to_string()));

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

        self.try_send(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "turn/interrupt",
            "params": {"threadId": thread_id, "turnId": turn_id},
        }))
        .is_ok()
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
            update: BackgroundTaskTranscriptUpdate::state(BackgroundTaskTranscriptState::Loading),
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

    /// Answer the pending approval request (`"accept"` / `"decline"`); a no-op
    /// when none is pending.
    pub fn respond_approval(&mut self, decision: &str) -> bool {
        let Some(rpc_id) = self.conversation.pending_approval else {
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

        self.conversation.pending_approval = None;

        true
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
            "item/tool/requestUserInput" => self.on_question_request(rpc_id, &message["params"]),
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

                self.conversation.pending_approval = Some(rpc_id);

                vec![Event::ApprovalRequested { description }]
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
            Some(ControlOperation::Command(command)) => (Some(command), None),
            Some(ControlOperation::Query(kind)) => (None, Some(kind)),
            Some(ControlOperation::Other | ControlOperation::ThreadRequest) => (None, None),
            Some(ControlOperation::ThreadName)
                if message["error"]["data"]["requestTimedOut"].as_bool() == Some(true) =>
            {
                (None, None)
            }
            Some(ControlOperation::ThreadName) | None => return Vec::new(),
        };

        if let Some(events) = self.on_question_response(rpc_id, message) {
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

            return self.on_response_error(pending_command.as_deref(), query, error);
        }

        if let Some(command) = pending_command {
            if command == "goal" {
                // A live update can arrive before the command reply. Read the
                // current goal with revision protection instead of restoring
                // the older state captured in that reply.
                self.request_goal();

                return vec![Event::SlashCommandResult {
                    name: command,
                    outcome: SlashCommandOutcome::Completed { message: None },
                }];
            }

            return vec![Event::SlashCommandResult {
                outcome: codex_command_response(&command, None),
                name: command,
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

                self.send_query(
                    QueryKind::Models,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "model/list",
                        "params": {"limit": 100},
                    }),
                );

                // History for the empty-tab session list, over whatever scope
                // the tab last asked for.
                self.request_history(self.history_scope);

                self.start_descendant_discovery();

                self.finish_team_start(vec![Event::Ready(parse_thread_settings(result))])
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
                BackgroundTaskTranscriptUpdate::state(BackgroundTaskTranscriptState::Unavailable {
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

        // A server that keeps handing back the same cursor would page
        // forever, so a repeat ends discovery instead of looping.
        if let Some(cursor) = next_cursor.filter(|cursor| self.background.accept_cursor(cursor)) {
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
        pending_command: Option<&str>,
        query: Option<QueryKind>,
        error: &str,
    ) -> Vec<Event> {
        if let Some(command) = pending_command {
            if command == "compact" {
                self.conversation.compaction.reject_manual_request();
            }

            return vec![Event::SlashCommandResult {
                name: command.to_string(),
                outcome: codex_command_response(command, Some(error)),
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
            _ => error.to_string(),
        };

        vec![Event::Error {
            message,
            fatal: initial_resume_failed || query == Some(QueryKind::Start),
        }]
    }

    fn on_thread_switched(&mut self, result: &Value) -> Vec<Event> {
        self.control.reset_thread();

        self.retain_request_routes();

        self.conversation.pending_approval = None;

        self.conversation.compaction.reset_thread();

        self.conversation.questions = QuestionState::default();
        self.conversation.current_turn = None;
        self.conversation.thread_id = result["thread"]["id"].as_str().map(str::to_owned);
        self.initial_resume = None;
        self.conversation.plan_revision += 1;
        self.conversation.goal_revision += 1;

        self.request_goal();

        if let Some(path) = result["thread"]["path"].as_str() {
            let path = PathBuf::from(path);
            let deliver = self.deliver.clone();
            let thread_id = self.conversation.thread_id.clone();
            let revision = self.conversation.plan_revision;

            let _ = thread::Builder::new()
                .name("codex-plan-restore".into())
                .spawn(move || {
                    deliver(json!({"method": PLAN_RESTORED, "params": {
                        "threadId": thread_id, "revision": revision, "value": read_plan(&path)
                    }}));
                });
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

        if is_legacy_compaction_notification(method) {
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

        self.conversation.current_turn = None;
        self.conversation.pending_approval = None;
        self.conversation.questions = QuestionState::default();

        self.control.close();

        self.skill_refresh = SkillRefreshState::default();

        self.conversation.compaction.reset_thread();

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
        let _ = self.shutdown(Duration::from_millis(250), true);
    }
}
