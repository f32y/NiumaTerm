//! Codex `app-server` chat session: process lifecycle, JSON-RPC handshake,
//! and translation of the backend protocol into typed events for a chat UI.
//!
//! The app-server protocol is Codex's supported integration surface for
//! third-party UIs (it powers the VS Code extension). One `Session` owns one
//! `codex app-server` process and one conversation thread on it.

use std::collections::HashMap;
use std::mem::take;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
#[cfg(test)]
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

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
use crate::workspace::AgentWorkspace;
use crate::{CodexProviderConfig, LaunchConfig};

mod background_tasks;
mod compaction;
mod host;
mod options;
mod protocol;
mod skills;

use crate::codex::app_server::background_tasks::{CodexTasks, ThreadScope, notification_thread_id};
use crate::codex::app_server::compaction::{
    CompactionState, compaction_completed, compaction_started, is_legacy_compaction_notification,
};
use crate::codex::app_server::host::{CodexHost, HOST_EXIT_METHOD, RegistrationId};
pub use crate::codex::app_server::options::{
    APPROVAL_OPTIONS, APPROVAL_REVIEWER_OPTIONS, SANDBOX_OPTIONS,
};
#[cfg(test)]
use crate::codex::app_server::protocol::thread_start_params;
use crate::codex::app_server::protocol::{
    codex_command_request, codex_command_response, codex_user_input, delta_event,
    file_change_paths, initial_thread_request, parse_context_window_usage, parse_fork_checkpoints,
    parse_item, parse_models, parse_replay, parse_thread_settings, parse_thread_summaries,
    resumed_thread_events, skills_list_request, stringify_command, thread_list_params,
    thread_resume_params, turn_start_params,
};
#[cfg(test)]
use crate::codex::app_server::skills::parse_skill_catalog;
use crate::codex::app_server::skills::{SkillRefreshState, skill_catalog_from_response};

/// JSON-RPC ids for the fixed handshake requests; turn requests count up from
/// `FIRST_TURN_RPC_ID` so response routing can tell the phases apart.
const THREAD_START_RPC_ID: u64 = 2;
const MODEL_LIST_RPC_ID: u64 = 3;
const THREAD_LIST_RPC_ID: u64 = 4;
const THREAD_RESUME_RPC_ID: u64 = 5;
const THREAD_READ_RPC_ID: u64 = 6;
const THREAD_FORK_RPC_ID: u64 = 7;
const FIRST_TURN_RPC_ID: u64 = 100;
const PROVIDER_API_FIELD: &str = concat!("wi", "re_api");

/// First page size for the history list; enough to fill the visible list
/// several times over. `nextCursor` remains available for deeper paging.
const THREAD_LIST_LIMIT: u64 = 50;

/// Notifications that describe one thread's activity. They are routed by
/// thread id before parent handling, so a descendant's turn or item can never
/// change the parent's turn identity, running state, or transcript.
const THREAD_SCOPED_NOTIFICATIONS: [&str; 10] = [
    "turn/started",
    "turn/completed",
    "thread/status/changed",
    "thread/tokenUsage/updated",
    "item/started",
    "item/completed",
    "item/agentMessage/delta",
    "item/reasoning/summaryTextDelta",
    "item/commandExecution/outputDelta",
    "error",
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

#[derive(Default)]
struct TurnOutputUsage {
    latest_total: Option<u64>,
    baseline: Option<u64>,
}

impl TurnOutputUsage {
    fn begin_turn(&mut self) {
        self.baseline = self.latest_total;
    }

    fn finish_turn(&mut self) {
        self.baseline = None;
    }

    fn observe(&mut self, total: u64, last: u64, active: bool) -> Option<u64> {
        self.latest_total = Some(total);
        if !active {
            return None;
        }

        let inferred_baseline = total.saturating_sub(last);
        let baseline = self.baseline.get_or_insert(inferred_baseline);
        if total < *baseline {
            *baseline = inferred_baseline;
        }

        Some(total.saturating_sub(*baseline))
    }
}

struct PendingThreadName {
    thread_id: String,
    name: String,
}

pub struct Session {
    host: Option<Arc<CodexHost>>,
    registration_id: RegistrationId,
    detached: bool,
    next_rpc_id: u64,
    thread_id: Option<String>,
    current_turn: Option<String>,
    /// JSON-RPC id of the server→client approval request awaiting an answer.
    pending_approval: Option<u64>,
    /// Cursor for the next history page; `None` once the final page arrived.
    history_cursor: Option<String>,
    /// Command RPC responses are independent of turn ids. Tracking their
    /// request ids keeps command failures non-fatal to the live thread.
    pending_commands: HashMap<u64, String>,
    /// Name responses arrive independently of turn events. Retaining the
    /// requested value lets the UI publish only a name the server accepted.
    pending_thread_names: HashMap<u64, PendingThreadName>,
    history_scope: SessionScope,
    skill_refresh: SkillRefreshState,
    compaction: CompactionState,
    turn_output_usage: TurnOutputUsage,
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
        ]
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
            None,
            false,
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
            Some(thread_id),
            suppress_replay,
            deliver,
            on_stderr,
        )
    }

    fn spawn_inner(
        launch: &LaunchConfig,
        host_catalog: &[LaunchConfig],
        workspace: &AgentWorkspace,
        initial_resume: Option<String>,
        suppress_resume_replay: bool,
        deliver: impl Fn(Value) + Send + Sync + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        let thread_profile = ThreadProfile::from(launch);
        let host = CodexHost::acquire(launch, host_catalog, on_stderr)?;
        let registration_id = host.register(deliver);

        let mut session = Self {
            host: Some(host),
            registration_id,
            detached: false,
            next_rpc_id: FIRST_TURN_RPC_ID,
            thread_id: None,
            current_turn: None,
            pending_approval: None,
            history_cursor: None,
            pending_commands: HashMap::new(),
            pending_thread_names: HashMap::new(),
            history_scope: SessionScope::default(),
            skill_refresh: SkillRefreshState::default(),
            compaction: CompactionState::default(),
            turn_output_usage: TurnOutputUsage::default(),
            thread_profile,
            workspace: workspace.clone(),
            initial_resume,
            suppress_resume_replay,
            background: CodexTasks::default(),
        };
        session.request_skills(false);
        let initial_request = initial_thread_request(
            session.initial_resume.as_deref(),
            &session.thread_profile,
            &session.workspace,
        );
        session.send(initial_request);

        Ok(session)
    }

    pub fn thread_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }

    pub fn has_active_operation(&self) -> bool {
        self.current_turn.is_some()
            || self.pending_approval.is_some()
            || !self.pending_commands.is_empty()
            || self.compaction.active.is_some()
    }

    /// Detach this thread from the shared host. The final owner performs the
    /// requested bounded process shutdown.
    pub fn shutdown(&mut self, timeout: Duration, force: bool) -> Result<(), String> {
        self.detach_with(timeout, force)
    }

    fn detach(&mut self) {
        let _ = self.detach_with(Duration::from_millis(250), true);
    }

    fn detach_with(&mut self, timeout: Duration, force: bool) -> Result<(), String> {
        if self.detached {
            return Ok(());
        }
        if let (Some(thread_id), Some(turn_id)) =
            (self.thread_id.clone(), self.current_turn.clone())
        {
            let rpc_id = self.alloc_rpc_id();
            self.send(json!({
                "jsonrpc": "2.0",
                "id": rpc_id,
                "method": "turn/interrupt",
                "params": {"threadId": thread_id, "turnId": turn_id},
            }));
        }
        if let Some(thread_id) = self.thread_id.clone() {
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
        let id = message["id"].as_u64();
        let method = message["method"].as_str().map(str::to_owned);

        let events = match (id, method.as_deref()) {
            (Some(rpc_id), Some(method)) => self.process_server_request(rpc_id, method, &message),
            (Some(rpc_id), None) => self.process_response(rpc_id, &message),
            (None, Some(method)) => self.process_notification(method, &message["params"]),
            (None, None) => Vec::new(),
        };
        self.sync_descendant_owners();
        events
    }

    /// A message typed while a turn is running becomes a steer (mid-turn
    /// interjection); otherwise it starts the next turn carrying the settings
    /// as overrides.
    pub fn send_user_message(&mut self, text: &str, settings: &ThreadSettings) -> SendOutcome {
        self.send_user_message_with_skill(text, settings, None, &[])
    }

    /// Send text plus the exact skill identity selected by a client picker.
    /// Text-only callers keep the original one-item request shape.
    /// Send text plus the exact skill identity selected by a client picker,
    /// and the local images the message carries. The server reads each image
    /// from the path given, so the caller keeps the file readable until the
    /// request has been written.
    pub fn send_user_message_with_skill(
        &mut self,
        text: &str,
        settings: &ThreadSettings,
        skill: Option<&SkillReference>,
        images: &[PathBuf],
    ) -> SendOutcome {
        let Some(thread_id) = self.thread_id.clone() else {
            return SendOutcome::NotReady;
        };

        let rpc_id = self.alloc_rpc_id();
        let input = codex_user_input(text, skill, images);

        if let Some(turn_id) = self.current_turn.clone() {
            self.send(json!({
                "jsonrpc": "2.0",
                "id": rpc_id,
                "method": "turn/steer",
                "params": {
                    "threadId": thread_id,
                    "expectedTurnId": turn_id,
                    "input": input,
                },
            }));

            return SendOutcome::Steered;
        }

        let params = turn_start_params(&thread_id, input, settings, &self.workspace);

        self.send(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "turn/start",
            "params": params,
        }));

        SendOutcome::StartedTurn
    }

    /// Store the first conversation name before submitting its first turn.
    /// Codex serializes both requests per thread, while accepting `turn/start`
    /// does not wait for the rollout's initial metadata to reach disk.
    pub fn send_user_message_with_thread_name(
        &mut self,
        text: &str,
        settings: &ThreadSettings,
        skill: Option<&SkillReference>,
        images: &[PathBuf],
        thread_name: &str,
    ) -> SendOutcome {
        self.set_thread_name(thread_name);
        self.send_user_message_with_skill(text, settings, skill, images)
    }

    /// Execute Codex operations that map directly to dedicated app-server
    /// requests. `/skills` is handled entirely by the UI picker and never
    /// reaches this method.
    pub fn execute_slash_command(&mut self, name: &str, arguments: &str) -> SlashCommandOutcome {
        let Some(thread_id) = self.thread_id.clone() else {
            return SlashCommandOutcome::NotReady;
        };
        if self.current_turn.is_some() {
            return SlashCommandOutcome::Rejected {
                message: "Codex is already running a turn.".to_string(),
            };
        }
        if !arguments.trim().is_empty() {
            return SlashCommandOutcome::Rejected {
                message: format!("/{name} does not accept arguments."),
            };
        }

        let rpc_id = self.alloc_rpc_id();
        let Some(request) = codex_command_request(rpc_id, &thread_id, name) else {
            return SlashCommandOutcome::Rejected {
                message: format!("Unsupported Codex command: /{name}"),
            };
        };

        if name == "compact" {
            self.compaction.request_manual();
        }
        self.pending_commands.insert(rpc_id, name.to_string());
        self.send(request);

        SlashCommandOutcome::Accepted
    }

    /// Interrupt the running turn (the Esc/Ctrl-C equivalent).
    pub fn interrupt(&mut self) {
        let (Some(thread_id), Some(turn_id)) = (self.thread_id.clone(), self.current_turn.clone())
        else {
            return;
        };

        let rpc_id = self.alloc_rpc_id();

        self.send(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "turn/interrupt",
            "params": {"threadId": thread_id, "turnId": turn_id},
        }));
    }

    /// Switch this session onto a persisted thread. The response carries the
    /// reconstructed turn history (emitted as [`Event::Replay`]) and the
    /// thread's persisted settings (emitted as [`Event::Ready`]); subsequent
    /// `turn/start` calls append to the resumed thread. On failure the
    /// session keeps the thread it started with, so the tab stays usable.
    pub fn resume_thread(&mut self, thread_id: &str) {
        self.compaction.reset_thread();
        let params = thread_resume_params(thread_id, &self.thread_profile);
        self.send(json!({
            "jsonrpc": "2.0",
            "id": THREAD_RESUME_RPC_ID,
            "method": "thread/resume",
            "params": params,
        }));
    }

    /// Ask which prompts this conversation can be branched in front of.
    ///
    /// The thread's own history answers it, so the list covers turns from
    /// before this session resumed the thread as well as the ones it watched
    /// run. Reading it per request rather than accumulating it as turns go by
    /// also keeps the offer honest after a compaction rewrites the thread.
    pub fn request_fork_checkpoints(&mut self) -> bool {
        let Some(thread_id) = self.thread_id.clone() else {
            return false;
        };

        self.send(json!({
            "jsonrpc": "2.0",
            "id": THREAD_READ_RPC_ID,
            "method": "thread/read",
            "params": {"threadId": thread_id, "includeTurns": true},
        }));
        true
    }

    /// Branch the thread at `anchor` and move this session onto the copy.
    ///
    /// The reply carries the same reconstructed history and persisted settings
    /// `thread/resume` answers with, so it is read by the same handler and the
    /// tab lands in the branch exactly as it lands in a resumed conversation.
    /// The source thread is left untouched.
    pub fn fork_thread(&mut self, anchor: &ForkAnchor) -> Result<(), String> {
        let ForkAnchor::CodexThrough(last_turn_id) = anchor else {
            return Err("that branch point belongs to another agent".to_string());
        };
        let Some(thread_id) = self.thread_id.clone() else {
            return Err("this conversation has no thread to branch".to_string());
        };

        self.compaction.reset_thread();
        let mut params = thread_resume_params(&thread_id, &self.thread_profile);
        params["lastTurnId"] = json!(last_turn_id);
        self.send(json!({
            "jsonrpc": "2.0",
            "id": THREAD_FORK_RPC_ID,
            "method": "thread/fork",
            "params": params,
        }));
        Ok(())
    }

    /// Name this thread. The server has no naming of its own — it stores what
    /// a client tells it and echoes the change to other clients — so the name
    /// comes from the caller. Fire and forget: the response says only whether
    /// the name was stored, which changes nothing this side shows.
    pub fn set_thread_name(&mut self, name: &str) {
        let Some(thread_id) = self.thread_id.clone() else {
            return;
        };
        if self
            .pending_thread_names
            .values()
            .any(|pending| pending.thread_id == thread_id)
        {
            return;
        }

        let rpc_id = self.alloc_rpc_id();
        self.pending_thread_names.insert(
            rpc_id,
            PendingThreadName {
                thread_id: thread_id.clone(),
                name: name.to_string(),
            },
        );

        self.send(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "thread/name/set",
            "params": {"threadId": thread_id, "name": name},
        }));
    }

    /// Ask for the first page of session history over `scope`. Replaces
    /// whatever an earlier scope was paging through, so the caller drops the
    /// rows it already holds.
    pub fn request_history(&mut self, scope: SessionScope) {
        self.history_scope = scope;
        self.history_cursor = None;

        let params = thread_list_params(&self.thread_profile, None, scope, &self.workspace);
        self.send(json!({
            "jsonrpc": "2.0",
            "id": THREAD_LIST_RPC_ID,
            "method": "thread/list",
            "params": params,
        }));
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
        self.send(json!({
            "jsonrpc": "2.0",
            "id": THREAD_LIST_RPC_ID,
            "method": "thread/list",
            "params": params,
        }));
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
        self.send(request);
        true
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
        let Some(thread_id) = self.thread_id.clone() else {
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
    pub fn respond_approval(&mut self, decision: &str) {
        let Some(rpc_id) = self.pending_approval.take() else {
            return;
        };

        self.send(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "result": {"decision": decision},
        }));
    }

    fn alloc_rpc_id(&mut self) -> u64 {
        let id = self.next_rpc_id;

        self.next_rpc_id += 1;

        id
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

    /// Write one request line; write failures stay unsurfaced because the
    /// reader-side EOF is the single exit-detection path.
    fn send(&mut self, message: Value) {
        let Some(host) = &self.host else {
            return;
        };
        if let Err(error) = host.send(self.registration_id, message) {
            tracing::warn!("could not write Codex app-server request: {error}");
        }
    }

    fn process_server_request(&mut self, rpc_id: u64, method: &str, message: &Value) -> Vec<Event> {
        match method {
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

                self.pending_approval = Some(rpc_id);

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

    fn process_response(&mut self, rpc_id: u64, message: &Value) -> Vec<Event> {
        if let Some(pending) = self.pending_thread_names.remove(&rpc_id) {
            if self.thread_id.as_deref() != Some(pending.thread_id.as_str()) {
                return Vec::new();
            }
            if let Some(error) = message["error"]["message"].as_str() {
                return vec![Event::Error {
                    message: error.to_string(),
                    fatal: false,
                }];
            }
            return vec![Event::TitleUpdated(pending.name)];
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
            let Some(thread_id) = self.background.finish_transcript_read(rpc_id) else {
                return Vec::new();
            };
            let key = BackgroundTaskKey::codex(&thread_id);
            let update = match message["error"]["message"].as_str() {
                Some(error) => BackgroundTaskTranscriptUpdate::state(
                    BackgroundTaskTranscriptState::Unavailable {
                        message: error.to_owned(),
                    },
                ),
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
            return vec![Event::BackgroundTaskTranscript { key, update }];
        }

        // Descendant discovery answers before the shared error path so a
        // failed page reports unavailable status instead of a session error.
        if self.background.is_query(rpc_id) {
            if let Some(error) = message["error"]["message"].as_str() {
                let changed = self.background.fail_query(rpc_id, error);
                return self.background_events(changed);
            }

            let (mut changed, next_cursor) = self
                .background
                .apply_descendants(rpc_id, &message["result"]);
            // A server that keeps handing back the same cursor would page
            // forever, so a repeat ends discovery instead of looping.
            if let Some(cursor) = next_cursor.filter(|cursor| self.background.accept_cursor(cursor))
            {
                let next_rpc_id = self.alloc_rpc_id();
                if let Some(request) = self
                    .background
                    .descendant_request(next_rpc_id, Some(&cursor))
                {
                    self.send(request);
                    changed = true;
                }
            }
            return self.background_events(changed);
        }

        let pending_command = self.pending_commands.remove(&rpc_id);
        let is_command = pending_command.is_some();

        if let Some(error) = message["error"]["message"].as_str() {
            if let Some(command) = pending_command.as_deref() {
                if command == "compact" {
                    self.compaction.reject_manual_request();
                }
                return vec![Event::SlashCommandResult {
                    name: command.to_string(),
                    outcome: codex_command_response(command, Some(error)),
                }];
            }

            // A branch-point list nobody could read leaves the picker with
            // nothing to show, which is the picker's own failure to report
            // rather than something that happened to the conversation.
            if rpc_id == THREAD_READ_RPC_ID {
                return vec![Event::ForkCheckpoints(Err(error.to_string()))];
            }

            // A failed resume (deleted/corrupt thread) is not fatal: the
            // session still has the thread it started with, so the composer
            // keeps working for a fresh conversation.
            let initial_resume_failed =
                rpc_id == THREAD_RESUME_RPC_ID && self.initial_resume.is_some();
            let message = match rpc_id {
                THREAD_RESUME_RPC_ID => format!("Could not resume session: {error}"),
                // A refused branch leaves the session on the thread it was
                // already holding, so the conversation stays usable.
                THREAD_FORK_RPC_ID => format!("Could not branch this conversation: {error}"),
                _ => error.to_string(),
            };

            return vec![Event::Error {
                message,
                fatal: initial_resume_failed || (!is_command && rpc_id <= THREAD_START_RPC_ID),
            }];
        }

        if let Some(command) = pending_command {
            return vec![Event::SlashCommandResult {
                outcome: codex_command_response(&command, None),
                name: command,
            }];
        }

        match rpc_id {
            THREAD_START_RPC_ID => {
                let result = &message["result"];

                self.thread_id = result["thread"]["id"].as_str().map(str::to_owned);

                self.send(json!({
                    "jsonrpc": "2.0",
                    "id": MODEL_LIST_RPC_ID,
                    "method": "model/list",
                    "params": {"limit": 100},
                }));
                // History for the empty-tab session list, over whatever scope
                // the tab last asked for.
                self.request_history(self.history_scope);
                self.start_descendant_discovery();

                vec![Event::Ready(parse_thread_settings(result))]
            }
            MODEL_LIST_RPC_ID => {
                let models = if self.thread_profile.provider.is_some() {
                    parse_models(&json!({"data": []}), self.thread_profile.model.as_deref())
                } else {
                    parse_models(&message["result"], self.thread_profile.model.as_deref())
                };
                vec![Event::Models(models)]
            }
            THREAD_LIST_RPC_ID => {
                let result = &message["result"];

                self.history_cursor = result["nextCursor"].as_str().map(str::to_owned);

                // The thread this session just started is part of the request
                // listing but is the tab's own live (empty) thread, and a
                // history row for it would resume a conversation the user is
                // already in.
                vec![Event::History(parse_thread_summaries(
                    result,
                    self.thread_id.as_deref(),
                ))]
            }
            THREAD_READ_RPC_ID => vec![Event::ForkCheckpoints(Ok(parse_fork_checkpoints(
                &message["result"]["thread"]["turns"],
            )))],
            // A branch answers with the same payload a resume answers with,
            // down to the settings block, so both switch this session onto the
            // thread the reply names.
            THREAD_RESUME_RPC_ID | THREAD_FORK_RPC_ID => {
                let result = &message["result"];

                self.thread_id = result["thread"]["id"].as_str().map(str::to_owned);
                self.initial_resume = None;
                // A resumed parent can already have finished descendants, and
                // a reconnect resumes into a new process with none of the live
                // child state the previous one observed.
                self.start_descendant_discovery();
                resumed_thread_events(result, take(&mut self.suppress_resume_replay))
            }
            _ => Vec::new(),
        }
    }

    fn process_notification(&mut self, method: &str, params: &Value) -> Vec<Event> {
        if method == HOST_EXIT_METHOD {
            self.current_turn = None;
            self.pending_approval = None;
            self.pending_commands.clear();
            self.compaction.reset_thread();
            return vec![Event::HostExited {
                message: params["message"]
                    .as_str()
                    .unwrap_or("Codex app-server stopped unexpectedly")
                    .to_string(),
            }];
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
                        .apply_descendant_notification(&thread_id, method, params);
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

        match method {
            "skills/changed" => {
                self.request_skills(true);
                Vec::new()
            }
            "turn/started" => {
                self.current_turn = params["turn"]["id"].as_str().map(str::to_owned);
                self.turn_output_usage.begin_turn();

                vec![Event::TurnStarted]
            }
            "turn/completed" => {
                self.current_turn = None;
                self.turn_output_usage.finish_turn();

                let error = (params["turn"]["status"].as_str() == Some("failed"))
                    .then(|| params["turn"]["error"]["message"].as_str())
                    .flatten()
                    .map(str::to_owned);
                self.compaction.clear_incomplete();

                vec![Event::TurnCompleted { error }]
            }
            "thread/tokenUsage/updated" => {
                let Some(usage) = parse_context_window_usage(&params["tokenUsage"]) else {
                    return Vec::new();
                };

                self.compaction.update_usage(usage);
                let active = params["turnId"]
                    .as_str()
                    .is_some_and(|turn_id| self.current_turn.as_deref() == Some(turn_id));
                let turn_output_tokens = usage
                    .cumulative
                    .and_then(|usage| usage.breakdown.output_tokens)
                    .zip(usage.current.output_tokens)
                    .and_then(|(total, last)| self.turn_output_usage.observe(total, last, active));

                let mut events = vec![Event::ContextWindowUpdated(usage)];
                if let Some(output_tokens) = turn_output_tokens {
                    events.push(Event::TurnOutputTokensUpdated(output_tokens));
                }
                events
            }
            "item/started" => {
                let item = &params["item"];
                if item["type"].as_str() == Some("contextCompaction") {
                    return compaction_started(&mut self.compaction, item);
                }

                // Collaboration items are the parent's own tool calls, so they
                // keep their transcript row; they additionally identify the
                // child thread the panel tracks.
                let changed = self.background.observe_parent_item(item);
                let mut events: Vec<Event> = parse_item(item)
                    .map(Event::ItemStarted)
                    .into_iter()
                    .collect();
                events.extend(self.background_events(changed));
                events
            }
            "item/completed" => {
                let item = &params["item"];
                if item["type"].as_str() == Some("contextCompaction") {
                    return compaction_completed(&mut self.compaction, item);
                }

                let changed = self.background.observe_parent_item(item);
                let mut events: Vec<Event> = parse_item(item)
                    .map(Event::ItemCompleted)
                    .into_iter()
                    .collect();
                events.extend(self.background_events(changed));
                events
            }
            "item/agentMessage/delta" => delta_event(params, |item_id, delta| {
                Event::AgentMessageDelta { item_id, delta }
            }),
            "item/reasoning/summaryTextDelta" => delta_event(params, |item_id, delta| {
                Event::ReasoningSummaryDelta { item_id, delta }
            }),
            "item/commandExecution/outputDelta" => delta_event(params, |item_id, delta| {
                Event::CommandOutputDelta { item_id, delta }
            }),
            "serverRequest/resolved" => {
                // Fires when a pending approval is answered or cleared by
                // turn lifecycle — tear down the approval UI either way.
                if self.pending_approval.is_some()
                    && self.pending_approval == params["requestId"].as_u64()
                {
                    self.pending_approval = None;

                    return vec![Event::ApprovalResolved];
                }

                Vec::new()
            }
            "error" => {
                let message = params["error"]["message"]
                    .as_str()
                    .or_else(|| params["message"].as_str())
                    .unwrap_or("unknown Codex error")
                    .to_string();

                vec![Event::Error {
                    message,
                    fatal: false,
                }]
            }
            // The status carries a protocol token rather than a sentence, and
            // the working row it would reach shows text to the user. Child-agent
            // rows read the same notification through their own reducer, which
            // maps it to a lifecycle state instead of showing the word.
            "thread/status/changed" => Vec::new(),
            _ => Vec::new(),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.detach();
    }
}

#[cfg(test)]
mod tests;
