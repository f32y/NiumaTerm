//! Codex `app-server` chat session: process lifecycle, JSON-RPC handshake,
//! and translation of the backend protocol into typed events for a chat UI.
//!
//! The app-server protocol is Codex's supported integration surface for
//! third-party UIs (it powers the VS Code extension). One `Session` owns one
//! `codex app-server` process and one conversation thread on it.

use std::collections::HashMap;
use std::mem::take;
use std::time::Duration;
#[cfg(test)]
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

pub use crate::chat::{
    Compaction, CompactionTrigger, ContextUsageScope, ContextWindowUsage, Event, Item, ModelInfo,
    ScopedTokenUsage, SendOutcome, SessionSummary, SkillCatalog, SkillInfo, SkillReference,
    SlashCommandArguments, SlashCommandInfo, SlashCommandOutcome, SlashCommandRunPolicy,
    SlashCommandSource, ThreadSettings, TokenUsageBreakdown,
};
use crate::launcher::AgentCli;
use crate::subprocess::JsonLineProcess;
use crate::{CodexProviderConfig, LaunchConfig};

mod compaction;
mod options;
mod protocol;
mod skills;

use crate::codex::app_server::compaction::{
    CompactionState, compaction_completed, compaction_started, is_legacy_compaction_notification,
};
pub use crate::codex::app_server::options::{
    APPROVAL_OPTIONS, APPROVAL_REVIEWER_OPTIONS, EFFORT_OPTIONS, SANDBOX_OPTIONS,
};
use crate::codex::app_server::protocol::{
    codex_command_request, codex_command_response, codex_user_input, delta_event,
    file_change_paths, initial_thread_request, parse_context_window_usage, parse_item,
    parse_models, parse_thread_settings, parse_thread_summaries, resumed_thread_events,
    skills_list_request, stringify_command, thread_list_params, thread_resume_params,
    turn_start_params,
};
#[cfg(test)]
use crate::codex::app_server::protocol::{parse_replay, thread_start_params};
#[cfg(test)]
use crate::codex::app_server::skills::parse_skill_catalog;
use crate::codex::app_server::skills::{SkillRefreshState, skill_catalog_from_response};

/// JSON-RPC ids for the fixed handshake requests; turn requests count up from
/// `FIRST_TURN_RPC_ID` so response routing can tell the phases apart.
const INIT_RPC_ID: u64 = 1;
const THREAD_START_RPC_ID: u64 = 2;
const MODEL_LIST_RPC_ID: u64 = 3;
const THREAD_LIST_RPC_ID: u64 = 4;
const THREAD_RESUME_RPC_ID: u64 = 5;
const FIRST_TURN_RPC_ID: u64 = 100;
const PROVIDER_API_FIELD: &str = concat!("wi", "re_api");

/// First page size for the history list; enough to fill the visible list
/// several times over. `nextCursor` remains available for deeper paging.
const THREAD_LIST_LIMIT: u64 = 50;

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

pub struct Session {
    process: JsonLineProcess,
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
    skill_refresh: SkillRefreshState,
    compaction: CompactionState,
    turn_output_usage: TurnOutputUsage,
    /// Profile-level model/provider overrides reused for thread start, history
    /// filtering, and resume. Provider credentials remain only in process env.
    thread_profile: ThreadProfile,
    initial_resume: Option<String>,
    suppress_resume_replay: bool,
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
        ]
    }

    /// Spawn `codex app-server` with piped stdio and send the `initialize`
    /// request. Every parsed stdout line is handed to `deliver` (from a
    /// reader thread — hop threads before calling [`Session::process`]);
    /// stderr lines go to `on_stderr`. Dropping `deliver` signals EOF: the
    /// closure is owned by the reader thread and dropped when the pipe closes.
    pub fn spawn(
        launch: &LaunchConfig,
        cwd: Option<String>,
        deliver: impl Fn(Value) + Send + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        Self::spawn_inner(launch, cwd, None, false, deliver, on_stderr)
    }

    /// Start app-server directly into an existing thread. The initialize
    /// handshake sends `thread/resume` instead of creating a disposable empty
    /// thread first; replay can be suppressed when the caller already retains
    /// the visible transcript in place.
    pub fn spawn_resuming(
        launch: &LaunchConfig,
        cwd: Option<String>,
        thread_id: String,
        suppress_replay: bool,
        deliver: impl Fn(Value) + Send + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        Self::spawn_inner(
            launch,
            cwd,
            Some(thread_id),
            suppress_replay,
            deliver,
            on_stderr,
        )
    }

    fn spawn_inner(
        launch: &LaunchConfig,
        cwd: Option<String>,
        initial_resume: Option<String>,
        suppress_resume_replay: bool,
        deliver: impl Fn(Value) + Send + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        let thread_profile = ThreadProfile::from(launch);
        let launcher = AgentCli::from_launch(launch, "codex");
        let executable = launcher.executable().to_string();
        let mut command = launcher.command(["app-server"]);

        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }

        let process = JsonLineProcess::spawn(
            command,
            &format!("{executable} app-server"),
            "Codex",
            deliver,
            on_stderr,
        )?;

        let mut session = Self {
            process,
            next_rpc_id: FIRST_TURN_RPC_ID,
            thread_id: None,
            current_turn: None,
            pending_approval: None,
            history_cursor: None,
            pending_commands: HashMap::new(),
            skill_refresh: SkillRefreshState::default(),
            compaction: CompactionState::default(),
            turn_output_usage: TurnOutputUsage::default(),
            thread_profile,
            initial_resume,
            suppress_resume_replay,
        };

        session.send(json!({
            "jsonrpc": "2.0",
            "id": INIT_RPC_ID,
            "method": "initialize",
            "params": {"clientInfo": {"name": "NiumaTerm", "version": "0.1.0"}},
        }));

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

    /// Close the protocol input and wait for the owned process tree to exit.
    /// Forced termination is opt-in because it can interrupt an active tool
    /// operation; dropping the Job Object affects only this session's tree.
    pub fn shutdown(&mut self, timeout: Duration, force: bool) -> Result<(), String> {
        self.process.shutdown(timeout, force)
    }

    /// Handle one message from the server: advances the handshake, answers
    /// protocol-level requests, and returns the events a chat UI reacts to.
    pub fn process(&mut self, message: Value) -> Vec<Event> {
        let id = message["id"].as_u64();
        let method = message["method"].as_str().map(str::to_owned);

        match (id, method.as_deref()) {
            (Some(rpc_id), Some(method)) => self.process_server_request(rpc_id, method, &message),
            (Some(rpc_id), None) => self.process_response(rpc_id, &message),
            (None, Some(method)) => self.process_notification(method, &message["params"]),
            (None, None) => Vec::new(),
        }
    }

    /// A message typed while a turn is running becomes a steer (mid-turn
    /// interjection); otherwise it starts the next turn carrying the settings
    /// as overrides.
    pub fn send_user_message(&mut self, text: &str, settings: &ThreadSettings) -> SendOutcome {
        self.send_user_message_with_skill(text, settings, None)
    }

    /// Send text plus the exact skill identity selected by a client picker.
    /// Text-only callers keep the original one-item request shape.
    pub fn send_user_message_with_skill(
        &mut self,
        text: &str,
        settings: &ThreadSettings,
        skill: Option<&SkillReference>,
    ) -> SendOutcome {
        let Some(thread_id) = self.thread_id.clone() else {
            return SendOutcome::NotReady;
        };

        let rpc_id = self.alloc_rpc_id();
        let input = codex_user_input(text, skill);

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

        let params = turn_start_params(&thread_id, input, settings);

        self.send(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "turn/start",
            "params": params,
        }));

        SendOutcome::StartedTurn
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

    /// Request the next history page; a no-op when the final page arrived.
    pub fn request_more_history(&mut self) {
        let Some(cursor) = self.history_cursor.take() else {
            return;
        };

        let params = thread_list_params(&self.thread_profile, Some(&cursor));
        self.send(json!({
            "jsonrpc": "2.0",
            "id": THREAD_LIST_RPC_ID,
            "method": "thread/list",
            "params": params,
        }));
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
        self.send(skills_list_request(rpc_id, force_reload));
    }

    /// Write one request line; write failures stay unsurfaced because the
    /// reader-side EOF is the single exit-detection path.
    fn send(&mut self, message: Value) {
        self.process.write_line(&message);
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
        if self.skill_refresh.in_flight == Some(rpc_id) {
            let catalog = skill_catalog_from_response(message);
            let force_reload_again = self.skill_refresh.finish(rpc_id).unwrap_or(false);

            if force_reload_again {
                self.request_skills(true);
            }

            return vec![Event::Skills(catalog)];
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

            // A failed resume (deleted/corrupt thread) is not fatal: the
            // session still has the thread it started with, so the composer
            // keeps working for a fresh conversation.
            let initial_resume_failed =
                rpc_id == THREAD_RESUME_RPC_ID && self.initial_resume.is_some();
            let message = if rpc_id == THREAD_RESUME_RPC_ID {
                format!("Could not resume session: {error}")
            } else {
                error.to_string()
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
            INIT_RPC_ID => {
                self.send(json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}));
                self.request_skills(false);
                self.send(initial_thread_request(
                    self.initial_resume.as_deref(),
                    &self.thread_profile,
                ));

                Vec::new()
            }
            THREAD_START_RPC_ID => {
                let result = &message["result"];

                self.thread_id = result["thread"]["id"].as_str().map(str::to_owned);

                self.send(json!({
                    "jsonrpc": "2.0",
                    "id": MODEL_LIST_RPC_ID,
                    "method": "model/list",
                    "params": {"limit": 100},
                }));
                // History for the empty-tab session list. `"."` resolves
                // against the app-server process cwd — the same directory
                // the started thread runs in — and the exact-match filter
                // keeps other projects' threads out.
                let history_params = thread_list_params(&self.thread_profile, None);
                self.send(json!({
                    "jsonrpc": "2.0",
                    "id": THREAD_LIST_RPC_ID,
                    "method": "thread/list",
                    "params": history_params,
                }));

                vec![Event::Ready(parse_thread_settings(result))]
            }
            MODEL_LIST_RPC_ID => vec![Event::Models(parse_models(
                &message["result"],
                self.thread_profile.model.as_deref(),
            ))],
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
            THREAD_RESUME_RPC_ID => {
                let result = &message["result"];

                self.thread_id = result["thread"]["id"].as_str().map(str::to_owned);
                self.initial_resume = None;
                resumed_thread_events(result, take(&mut self.suppress_resume_replay))
            }
            _ => Vec::new(),
        }
    }

    fn process_notification(&mut self, method: &str, params: &Value) -> Vec<Event> {
        if is_legacy_compaction_notification(method) {
            // Current servers can publish this deprecated notification beside
            // the authoritative item lifecycle. Ignoring it prevents a second
            // boundary for the same context rewrite.
            return Vec::new();
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

                parse_item(item)
                    .map(Event::ItemStarted)
                    .into_iter()
                    .collect()
            }
            "item/completed" => {
                let item = &params["item"];
                if item["type"].as_str() == Some("contextCompaction") {
                    return compaction_completed(&mut self.compaction, item);
                }

                parse_item(item)
                    .map(Event::ItemCompleted)
                    .into_iter()
                    .collect()
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
            "thread/status/changed" => vec![Event::StatusDetail(
                params["status"]["type"].as_str().map(str::to_owned),
            )],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests;
