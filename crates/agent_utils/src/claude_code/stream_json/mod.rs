//! Claude Code stream-json chat session: process lifecycle, control-protocol
//! handshake, and translation of the backend protocol into typed events for a
//! chat UI.
//!
//! The protocol is the one the official Claude Agent SDK speaks to the CLI:
//! `claude -p --input-format stream-json --output-format stream-json` with
//! newline-delimited JSON both ways. One `Session` owns one long-lived
//! `claude` process; multi-turn conversation is more user messages on stdin.
//! Permission prompts arrive as `control_request { can_use_tool }` because we
//! pass `--permission-prompt-tool stdio` (verified: approvals still fire with
//! `--allow-dangerously-skip-permissions` present — that flag only unlocks
//! switching into `bypassPermissions` mode).

use std::collections::{HashMap, VecDeque};
#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::process::Command;
use std::time::Duration;

use control::{
    PendingApproval, PendingControlOperation, fail_pending_control_operations,
    resolve_pending_control_operation,
};
#[cfg(test)]
use launch::{ANTHROPIC_MODEL_ENV, FILE_CHECKPOINTING_ENV};
use launch::{
    configured_permission_mode, enable_file_checkpointing, file_rewind_request,
    initial_ready_model, launch_model,
};
#[cfg(test)]
use parse::parse_slash_commands;
use parse::{
    approval_description, claude_context_window, claude_result_error, compaction_progress,
    context_window_usage, initialize_command_catalog, legacy_command_catalog, parse_claude_usage,
    parse_models, slash_command_text, ui_owns_slash_command, update_claude_output,
};
use serde_json::{Value, json};

use super::compaction::{compaction_metadata, parse_compaction};
use super::tool_items::{complete_tool_item, tool_item};
#[cfg(test)]
use super::tool_items::{edit_diff, input_detail};
use crate::LaunchConfig;
#[cfg(test)]
use crate::chat::ContextUsageScope;
use crate::chat::{
    ContextWindowUsage, Event, Item, SendOutcome, SlashCommandArguments, SlashCommandInfo,
    SlashCommandOutcome, SlashCommandRunPolicy, SlashCommandSource, ThreadSettings,
    TokenUsageBreakdown,
};
use crate::launcher::AgentCli;
use crate::subprocess::JsonLineProcess;

mod control;
mod launch;
mod parse;

/// Serialized values for `--permission-mode` / the `set_permission_mode` control
/// request. `auto` is the CLI's dynamic mode (verified accepted by
/// `set_permission_mode` on 2.1.222).
pub const PERMISSION_OPTIONS: [&str; 5] = [
    "default",
    "auto",
    "acceptEdits",
    "plan",
    "bypassPermissions",
];

const INIT_REQUEST_ID: &str = "nmt-init";

#[derive(Default)]
struct TurnOutputUsage {
    completed_responses: u64,
    current_response: Option<u64>,
}

impl TurnOutputUsage {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn start_response(&mut self, output_tokens: u64) -> u64 {
        self.completed_responses = self
            .completed_responses
            .saturating_add(self.current_response.take().unwrap_or(0));
        self.current_response = Some(output_tokens);
        self.total()
    }

    fn update_response(&mut self, output_tokens: u64) -> u64 {
        self.current_response = Some(output_tokens);
        self.total()
    }

    fn total(&self) -> u64 {
        self.completed_responses
            .saturating_add(self.current_response.unwrap_or(0))
    }
}

pub struct Session {
    process: JsonLineProcess,
    next_request_id: u64,
    ready: bool,
    /// The CLI's session id from the `init` message; the handle a future tab
    /// needs to `--resume` this conversation.
    session_id: Option<String>,
    turn_active: bool,
    /// The turn was started locally but no output has arrived yet; the first
    /// message after a send emits `TurnStarted` (the protocol has no explicit
    /// turn-started notification — `result` is the only turn boundary).
    turn_reported: bool,
    pending_approval: Option<PendingApproval>,
    /// Last model/permission actually applied by the backend, so settings picked
    /// in the UI turn into `set_model` / `set_permission_mode` control
    /// requests exactly when they change.
    applied_model: Option<String>,
    applied_permission: Option<String>,
    /// Streamed content blocks of the in-flight assistant message, keyed by
    /// their stream index, so text/thinking deltas route to transcript items.
    open_blocks: HashMap<u64, String>,
    /// Streamed text/thinking items not yet finalized by an `assistant`
    /// snapshot. Snapshots arrive per completed block in stream order, so
    /// FIFO matching by kind pairs each snapshot with its streamed item.
    open_texts: VecDeque<String>,
    open_thinkings: VecDeque<String>,
    /// Started tool items by `tool_use_id`; the matching `tool_result` block
    /// completes them with output and status.
    pending_tools: HashMap<String, Item>,
    item_seq: u64,
    active_slash_command: Option<String>,
    /// Control requests with user-visible completion semantics. Fire-and-forget
    /// settings requests are deliberately absent; only operations that the UI
    /// must await are correlated here.
    pending_control_operations: HashMap<String, PendingControlOperation>,
    /// A structured initialize catalog carries richer metadata than the
    /// string-only first-turn fallback and must remain authoritative.
    structured_commands_published: bool,
    /// The most recent assistant message's input/output accounting represents
    /// the live context, unlike result-level totals which may sum retries and
    /// tool-loop iterations.
    context_usage: Option<TokenUsageBreakdown>,
    last_turn_usage: Option<TokenUsageBreakdown>,
    context_window: Option<u64>,
    turn_output_usage: TurnOutputUsage,
    /// A compaction is running. Tracked because the CLI re-announces it every
    /// 30 seconds while a long compaction proceeds, and the UI only needs the
    /// state transitions.
    compacting: bool,
}

impl Session {
    /// Commands implemented by the Claude CLI but not necessarily included
    /// in every version's dynamic discovery payload.
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
                name: "rewind".to_string(),
                description: "Restore files or conversation to an earlier prompt".to_string(),
                argument_hint: None,
                source: SlashCommandSource::Adapter,
                arguments: SlashCommandArguments::None,
                run_policy: SlashCommandRunPolicy::IdleOnly,
            },
        ]
    }

    /// Spawn `claude` in bidirectional stream-json mode and send the SDK-style
    /// `initialize` control request. Every parsed stdout line is handed to
    /// `deliver` (from a reader thread — hop threads before calling
    /// [`Session::process`]); stderr lines go to `on_stderr`.
    ///
    /// With `resume`, the CLI reloads that persisted session and appends to
    /// it (same session id, same transcript file). Resume lookup is scoped to
    /// the project directory derived from `cwd`, so the id must come from a
    /// listing for the same directory. Nothing is replayed by the backend — the
    /// UI pre-fills its transcript from the session file instead.
    pub fn spawn(
        launch: &LaunchConfig,
        cwd: Option<String>,
        resume: Option<String>,
        deliver: impl Fn(Value) + Send + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        let initial_model = launch_model(launch);
        let launcher = AgentCli::from_launch(launch, "claude");
        let executable = launcher.executable().to_string();
        let mut command = launcher.command([
            "-p",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
            "--allow-dangerously-skip-permissions",
        ]);

        // File snapshots are opt-in for stream-json SDK clients. This is
        // applied after profile overrides so every NiumaTerm Claude session
        // can create checkpoints for subsequent `/rewind` operations.
        enable_file_checkpointing(&mut command);

        if let Some(session_id) = &resume {
            command.args(["--resume", session_id]);
        }

        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }

        let process = JsonLineProcess::spawn(command, &executable, "Claude", deliver, on_stderr)?;

        let mut session = Self {
            process,
            next_request_id: 1,
            ready: false,
            // A resumed process may not emit `system/init` until its next
            // model turn. The caller already obtained this identity from the
            // same cwd's history, so it is immediately valid for local
            // checkpoint lookup; a later init can still confirm or replace it.
            session_id: resume,
            turn_active: false,
            turn_reported: false,
            pending_approval: None,
            applied_model: initial_model,
            applied_permission: None,
            open_blocks: HashMap::new(),
            open_texts: VecDeque::new(),
            open_thinkings: VecDeque::new(),
            pending_tools: HashMap::new(),
            item_seq: 0,
            active_slash_command: None,
            pending_control_operations: HashMap::new(),
            structured_commands_published: false,
            context_usage: None,
            last_turn_usage: None,
            context_window: None,
            turn_output_usage: TurnOutputUsage::default(),
            compacting: false,
        };

        session.send(json!({
            "type": "control_request",
            "request_id": INIT_REQUEST_ID,
            "request": {"subtype": "initialize"},
        }));

        Ok(session)
    }

    pub fn has_active_operation(&self) -> bool {
        self.turn_active
            || self.pending_approval.is_some()
            || self.compacting
            || !self.pending_control_operations.is_empty()
    }

    /// Request EOF shutdown and wait for the launcher plus every contained
    /// descendant. Forced termination is used only after an explicit user
    /// choice to interrupt active work.
    pub fn shutdown(&mut self, timeout: Duration, force: bool) -> Result<(), String> {
        self.process.shutdown(timeout, force)
    }

    /// Handle one message from the CLI: answers control requests and returns
    /// the events a chat UI reacts to.
    pub fn process(&mut self, message: Value) -> Vec<Event> {
        let mut events = Vec::new();

        // First sign of life after a send: the turn is actually running.
        if self.turn_active && !self.turn_reported {
            self.turn_reported = true;
            events.push(Event::TurnStarted);
        }

        match message["type"].as_str() {
            Some("system") => events.extend(self.process_system(&message)),
            Some("stream_event") => events.extend(self.process_stream_event(&message)),
            Some("assistant") => events.extend(self.process_assistant(&message)),
            Some("user") => events.extend(self.process_tool_results(&message)),
            Some("result") => events.extend(self.process_result(&message)),
            Some("control_request") => events.extend(self.process_control_request(&message)),
            Some("control_response") => events.extend(self.process_control_response(&message)),
            Some("control_cancel_request") => {
                if self
                    .pending_approval
                    .as_ref()
                    .is_some_and(|p| Some(p.request_id.as_str()) == message["request_id"].as_str())
                {
                    self.pending_approval = None;
                    events.push(Event::ApprovalResolved);
                }
            }
            _ => {}
        }

        events
    }

    /// Write the user message, applying changed settings first via control
    /// requests (model and permission mode are session state on the CLI, so
    /// they are set once per change instead of per turn).
    pub fn send_user_message(&mut self, text: &str, settings: &ThreadSettings) -> SendOutcome {
        if !self.process.has_stdin() {
            return SendOutcome::NotReady;
        }

        if settings.model.is_some() && settings.model != self.applied_model {
            let model = settings.model.clone().unwrap_or_default();

            self.send_control(json!({"subtype": "set_model", "model": model}));
            self.applied_model = settings.model.clone();
        }
        if settings.approval.is_some() && settings.approval != self.applied_permission {
            let mode = settings.approval.clone().unwrap_or_default();

            self.send_control(json!({"subtype": "set_permission_mode", "mode": mode}));
            self.applied_permission = settings.approval.clone();
        }

        self.send(json!({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "text", "text": text}]},
        }));

        if self.turn_active {
            SendOutcome::Steered
        } else {
            self.turn_active = true;
            self.turn_reported = false;
            self.turn_output_usage.reset();

            SendOutcome::StartedTurn
        }
    }

    /// Send a provider command through Claude's stream-json command path.
    /// This intentionally bypasses `send_user_message`: the UI must not add
    /// a user bubble or steer a running model turn for slash commands.
    pub fn execute_slash_command(&mut self, name: &str, arguments: &str) -> SlashCommandOutcome {
        if ui_owns_slash_command(name) {
            return SlashCommandOutcome::Rejected {
                message: format!("/{name} is handled by NiumaTerm."),
            };
        }
        if !self.ready || !self.process.has_stdin() {
            return SlashCommandOutcome::NotReady;
        }
        if self.turn_active {
            return SlashCommandOutcome::Rejected {
                message: "Claude is already running a turn.".to_string(),
            };
        }

        let text = slash_command_text(name, arguments);

        self.send(json!({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "text", "text": text}]},
        }));
        self.turn_active = true;
        self.turn_reported = false;
        self.turn_output_usage.reset();
        self.active_slash_command = Some(name.to_string());

        SlashCommandOutcome::Accepted
    }

    /// Restore files tracked by Claude to the state captured before the user
    /// message. Completion arrives asynchronously as `FileRewindCompleted`.
    pub fn rewind_files(&mut self, user_message_id: &str) -> SlashCommandOutcome {
        if !self.ready || !self.process.has_stdin() {
            return SlashCommandOutcome::NotReady;
        }
        if self.turn_active || self.pending_approval.is_some() {
            return SlashCommandOutcome::Rejected {
                message: "Claude must be idle before restoring files.".to_string(),
            };
        }
        if self
            .pending_control_operations
            .values()
            .any(|operation| *operation == PendingControlOperation::FileRewind)
        {
            return SlashCommandOutcome::Rejected {
                message: "A Claude file restore is already running.".to_string(),
            };
        }

        let request_id = self.send_control(file_rewind_request(user_message_id));
        self.pending_control_operations
            .insert(request_id, PendingControlOperation::FileRewind);

        SlashCommandOutcome::Accepted
    }

    /// Resolve operations that can no longer receive a control response after
    /// stdout closes. The pane calls this before reporting the process exit.
    pub fn process_exit(&mut self) -> Vec<Event> {
        fail_pending_control_operations(
            &mut self.pending_control_operations,
            "Claude exited before file restore completed.",
        )
    }

    /// Interrupt the running turn (the Esc/Ctrl-C equivalent).
    pub fn interrupt(&mut self) {
        self.send_control(json!({"subtype": "interrupt"}));
    }

    /// The CLI's session id, known immediately for a resumed process and
    /// otherwise populated by its `init` message. This is what `spawn`'s
    /// `resume` takes to reopen the conversation later.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Answer the pending `can_use_tool` request. The UI decision vocabulary
    /// maps onto the CLI's allow/deny responses: `accept` allows once,
    /// `acceptForSession` allows and applies the CLI's own permission
    /// suggestions (e.g. switching to acceptEdits for the session), `decline`
    /// denies, and `cancel` denies and interrupts the turn.
    pub fn respond_approval(&mut self, decision: &str) {
        let Some(pending) = self.pending_approval.take() else {
            return;
        };

        let response = match decision {
            "accept" => json!({"behavior": "allow", "updatedInput": pending.input}),
            "acceptForSession" => {
                let mut response = json!({"behavior": "allow", "updatedInput": pending.input});

                if let Some(suggestions) = pending.suggestions {
                    response["updatedPermissions"] = suggestions;
                }

                response
            }
            "cancel" => json!({"behavior": "deny", "message": "User cancelled tool execution."}),
            _ => json!({"behavior": "deny", "message": "User declined tool execution."}),
        };

        self.send(json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": pending.request_id,
                "response": response,
            },
        }));

        if decision == "cancel" {
            self.interrupt();
        }
    }

    fn alloc_item_id(&mut self, prefix: &str) -> String {
        self.item_seq += 1;

        format!("{prefix}-{}", self.item_seq)
    }

    fn send_control(&mut self, request: Value) -> String {
        let request_id = format!("nmt-{}", self.next_request_id);

        self.next_request_id += 1;
        self.send(json!({
            "type": "control_request",
            "request_id": request_id,
            "request": request,
        }));

        request_id
    }

    /// Write one line; write failures stay unsurfaced because the reader-side
    /// EOF is the single exit-detection path.
    fn send(&mut self, message: Value) {
        self.process.write_line(&message);
    }

    fn process_system(&mut self, message: &Value) -> Vec<Event> {
        match message["subtype"].as_str() {
            Some("init") => self.process_init(message),
            Some("status") => self.process_status(message),
            Some("compact_boundary") => self.process_compact_boundary(message),
            // Every other subtype (hook_*, thinking_tokens, informational, …)
            // is telemetry the UI ignores.
            _ => Vec::new(),
        }
    }

    fn process_init(&mut self, message: &Value) -> Vec<Event> {
        // The session id makes this conversation resumable by a future tab
        // (`--resume`); captured on every `init` since a resumed session
        // keeps the id of the transcript it reloaded.
        if let Some(session_id) = message["session_id"].as_str() {
            self.session_id = Some(session_id.to_string());
        }

        // `init` — emitted when the first turn opens — carries the session's
        // ACTUAL permission mode, which the initialize response does not
        // (its value is this client's best guess from config). Always
        // applied: any user pick was already sent as a control request
        // before the message that opened this turn, so `init` reports the
        // post-change state and cannot clobber it. The model is only taken
        // before the handshake settled: `init` reports the resolved model id
        // (e.g. `claude-opus-5[1m]`), which is not a catalog value, so
        // adopting it later would break the catalog-driven picker display.
        let model = if self.ready {
            self.applied_model.clone()
        } else {
            message["model"].as_str().map(str::to_owned)
        };
        let permission = message["permissionMode"].as_str().map(str::to_owned);

        self.ready = true;
        self.applied_model = model.clone();
        self.applied_permission = permission.clone();

        let mut events = vec![Event::Ready(ThreadSettings {
            model,
            approval: permission,
            ..ThreadSettings::default()
        })];

        // Older Claude versions only reveal this string catalog when the
        // first turn opens. It must not erase richer initialize metadata.
        if let Some(commands) = legacy_command_catalog(
            self.structured_commands_published,
            &message["slash_commands"],
        ) {
            events.push(Event::Commands(commands));
        }

        events
    }

    fn process_status(&mut self, message: &Value) -> Vec<Event> {
        compaction_progress(&mut self.compacting, message)
    }

    /// The post-compaction boundary. Live it carries only the token accounting:
    /// the replacement summary is written to the transcript file and marked
    /// visible there only, so a resumed thread shows it and this one does not.
    fn process_compact_boundary(&mut self, message: &Value) -> Vec<Event> {
        let detail = parse_compaction(compaction_metadata(message));
        let id = match message["uuid"].as_str() {
            Some(uuid) => format!("compaction-{uuid}"),
            None => self.alloc_item_id("compaction"),
        };
        let post_tokens = detail.post_tokens;

        self.compacting = false;

        let mut events = vec![
            Event::CompactionFinished { error: None },
            Event::ItemCompleted(Item::Compaction { id, detail }),
        ];

        // Compaction replaces the prompt, so the live context is this size from
        // here on. Without the correction the gauge keeps showing the
        // pre-compaction total until the next assistant message reports usage,
        // which is exactly when the boundary row claims space was reclaimed.
        if let Some(post_tokens) = post_tokens {
            self.context_usage = Some(TokenUsageBreakdown::total_only(post_tokens));

            if let Some(usage) = self.context_window_usage() {
                events.push(Event::ContextWindowUpdated(usage));
            }
        }

        events
    }

    fn process_stream_event(&mut self, message: &Value) -> Vec<Event> {
        // Subagent (Task tool) internals stream with a parent id; the parent
        // tool row already represents them in the transcript.
        if !message["parent_tool_use_id"].is_null() {
            return Vec::new();
        }

        let event = &message["event"];
        let index = event["index"].as_u64();

        match event["type"].as_str() {
            Some("message_start") => {
                self.open_blocks.clear();
                self.open_texts.clear();
                self.open_thinkings.clear();

                self.context_usage = parse_claude_usage(&event["message"]["usage"]);
                let turn_output_tokens = self.turn_output_usage.start_response(
                    self.context_usage
                        .and_then(|usage| usage.output_tokens)
                        .unwrap_or(0),
                );

                let mut events = self
                    .context_window_usage()
                    .map(Event::ContextWindowUpdated)
                    .into_iter()
                    .collect::<Vec<_>>();
                events.push(Event::TurnOutputTokensUpdated(turn_output_tokens));

                events
            }
            Some("message_delta") => {
                let turn_output_tokens =
                    event["usage"]["output_tokens"]
                        .as_u64()
                        .map(|output_tokens| {
                            update_claude_output(&mut self.context_usage, output_tokens);
                            self.turn_output_usage.update_response(output_tokens)
                        });

                let mut events = self
                    .context_window_usage()
                    .map(Event::ContextWindowUpdated)
                    .into_iter()
                    .collect::<Vec<_>>();
                if let Some(output_tokens) = turn_output_tokens {
                    events.push(Event::TurnOutputTokensUpdated(output_tokens));
                }

                events
            }
            Some("content_block_start") => {
                let Some(index) = index else {
                    return Vec::new();
                };

                match event["content_block"]["type"].as_str() {
                    Some("text") => {
                        let id = self.alloc_item_id("text");

                        self.open_blocks.insert(index, id.clone());
                        self.open_texts.push_back(id.clone());

                        vec![Event::ItemStarted(Item::AgentMessage { id, text: None })]
                    }
                    Some("thinking") => {
                        let id = self.alloc_item_id("thinking");

                        self.open_blocks.insert(index, id.clone());
                        self.open_thinkings.push_back(id.clone());

                        vec![Event::ItemStarted(Item::Reasoning { id, summary: None })]
                    }
                    // Tool-use blocks stream their input as JSON fragments;
                    // the item is emitted from the `assistant` snapshot where
                    // the input is complete.
                    _ => Vec::new(),
                }
            }
            Some("content_block_delta") => {
                let Some(item_id) = index.and_then(|i| self.open_blocks.get(&i)).cloned() else {
                    return Vec::new();
                };
                let delta = &event["delta"];

                match delta["type"].as_str() {
                    Some("text_delta") => delta["text"]
                        .as_str()
                        .map(|text| Event::AgentMessageDelta {
                            item_id,
                            delta: text.to_string(),
                        })
                        .into_iter()
                        .collect(),
                    Some("thinking_delta") => delta["thinking"]
                        .as_str()
                        .map(|text| Event::ReasoningSummaryDelta {
                            item_id,
                            delta: text.to_string(),
                        })
                        .into_iter()
                        .collect(),
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    /// An `assistant` snapshot finalizes each content block it carries: text
    /// and thinking blocks overwrite their streamed item with the
    /// authoritative full text (or create it when partial messages were
    /// missed), tool-use blocks become started tool items.
    fn process_assistant(&mut self, message: &Value) -> Vec<Event> {
        if !message["parent_tool_use_id"].is_null() {
            return Vec::new();
        }

        let Some(blocks) = message["message"]["content"].as_array() else {
            return Vec::new();
        };
        let mut events = Vec::new();

        if let Some(usage) = parse_claude_usage(&message["message"]["usage"]) {
            self.context_usage = Some(usage);
            if let Some(snapshot) = self.context_window_usage() {
                events.push(Event::ContextWindowUpdated(snapshot));
            }
            if let Some(output_tokens) = usage.output_tokens {
                events.push(Event::TurnOutputTokensUpdated(
                    self.turn_output_usage.update_response(output_tokens),
                ));
            }
        }

        for block in blocks {
            match block["type"].as_str() {
                Some("text") => {
                    let id = self
                        .open_texts
                        .pop_front()
                        .unwrap_or_else(|| self.alloc_item_id("text"));

                    events.push(Event::ItemCompleted(Item::AgentMessage {
                        id,
                        text: block["text"].as_str().map(str::to_owned),
                    }));
                }
                Some("thinking") => {
                    let id = self
                        .open_thinkings
                        .pop_front()
                        .unwrap_or_else(|| self.alloc_item_id("thinking"));

                    events.push(Event::ItemCompleted(Item::Reasoning {
                        id,
                        summary: block["thinking"].as_str().map(str::to_owned),
                    }));
                }
                Some("tool_use") | Some("server_tool_use") | Some("mcp_tool_use") => {
                    let Some(id) = block["id"].as_str() else {
                        continue;
                    };
                    let item = tool_item(
                        id,
                        block["name"].as_str().unwrap_or("tool"),
                        &block["input"],
                    );

                    self.pending_tools.insert(id.to_string(), item.clone());
                    events.push(Event::ItemStarted(item));
                }
                _ => {}
            }
        }

        events
    }

    /// `user` messages in the stream carry tool results; each one completes
    /// its started tool item with output and success/failure status.
    fn process_tool_results(&mut self, message: &Value) -> Vec<Event> {
        let Some(blocks) = message["message"]["content"].as_array() else {
            return Vec::new();
        };
        let mut events = Vec::new();

        for block in blocks {
            if block["type"].as_str() != Some("tool_result") {
                continue;
            }
            let Some(id) = block["tool_use_id"].as_str() else {
                continue;
            };
            let Some(started) = self.pending_tools.remove(id) else {
                continue;
            };

            events.push(Event::ItemCompleted(complete_tool_item(started, block)));
        }

        events
    }

    fn process_result(&mut self, message: &Value) -> Vec<Event> {
        self.turn_active = false;
        self.turn_reported = false;

        let mut events = Vec::new();

        // A turn that ends with an unanswered approval (e.g. after an
        // interrupt) must tear down the approval card.
        if self.pending_approval.take().is_some() {
            events.push(Event::ApprovalResolved);
        }

        // Compaction only runs inside a turn, so a still-set flag here means
        // its end notification was lost (interrupt, aborted turn); the
        // indicator must not outlive the turn that owned it.
        if self.compacting {
            self.compacting = false;
            events.push(Event::CompactionFinished { error: None });
        }

        let error = claude_result_error(message);

        if let Some(name) = self.active_slash_command.take() {
            events.push(Event::SlashCommandResult {
                name,
                outcome: match error.as_ref() {
                    Some(message) => SlashCommandOutcome::Rejected {
                        message: message.clone(),
                    },
                    None => SlashCommandOutcome::Completed { message: None },
                },
            });
        }

        if let Some(max_tokens) = claude_context_window(&message["modelUsage"]) {
            self.context_window = Some(max_tokens);
        }
        self.last_turn_usage = parse_claude_usage(&message["usage"]);

        if let Some(usage) = self.context_window_usage() {
            events.push(Event::ContextWindowUpdated(usage));
        }
        if let Some(output_tokens) = self.last_turn_usage.and_then(|usage| usage.output_tokens) {
            events.push(Event::TurnOutputTokensUpdated(output_tokens));
        }

        events.push(Event::TurnCompleted { error });

        events
    }

    fn process_control_request(&mut self, message: &Value) -> Vec<Event> {
        let request_id = message["request_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let request = &message["request"];

        if request["subtype"].as_str() != Some("can_use_tool") {
            // Unsupported server→client requests (hook_callback, mcp_message,
            // …) get an error reply so the turn can't hang on them.
            self.send(json!({
                "type": "control_response",
                "response": {
                    "subtype": "error",
                    "request_id": request_id,
                    "error": "not supported by NiumaTerm agent tab",
                },
            }));

            return Vec::new();
        }

        let tool_name = request["tool_name"].as_str().unwrap_or("tool");

        // AskUserQuestion expects client-injected answers, and this UI has no
        // question form yet; a deny with guidance keeps the turn moving
        // instead of hanging it.
        if tool_name == "AskUserQuestion" {
            self.send(json!({
                "type": "control_response",
                "response": {
                    "subtype": "success",
                    "request_id": request_id,
                    "response": {
                        "behavior": "deny",
                        "message": "Interactive questions are not supported in this client; make your best assumption and continue.",
                    },
                },
            }));

            return Vec::new();
        }

        let description = approval_description(tool_name, &request["input"]);

        self.pending_approval = Some(PendingApproval {
            request_id,
            input: request["input"].clone(),
            suggestions: (!request["permission_suggestions"].is_null())
                .then(|| request["permission_suggestions"].clone()),
        });

        vec![Event::ApprovalRequested { description }]
    }

    fn process_control_response(&mut self, message: &Value) -> Vec<Event> {
        let response = &message["response"];

        if let Some(event) =
            resolve_pending_control_operation(&mut self.pending_control_operations, response)
        {
            return vec![event];
        }

        if response["subtype"].as_str() == Some("error") {
            let error = response["error"]
                .as_str()
                .unwrap_or("unknown Claude control error")
                .to_string();

            return vec![Event::Error {
                message: error,
                // A failed initialize means the CLI rejected this client.
                fatal: response["request_id"].as_str() == Some(INIT_REQUEST_ID),
            }];
        }

        // The initialize response arrives before any turn and carries the
        // model catalog, so the pickers show real values immediately. It
        // does NOT report the session's current permission mode, and the CLI
        // resolves its startup mode from user config — so the initial value
        // comes from the same config file (`permissions.defaultMode`); the
        // first turn's `init` message then confirms or corrects it. The
        // `ANTHROPIC_MODEL` fixes the model before the handshake; without it,
        // the session starts on the catalog's "default" entry because spawn
        // passes no `--model` argument.
        if response["request_id"].as_str() == Some(INIT_REQUEST_ID) && !self.ready {
            let permission =
                Some(configured_permission_mode().unwrap_or_else(|| "default".to_string()));
            let model = initial_ready_model(self.applied_model.as_deref());

            self.ready = true;
            self.applied_model = Some(model.clone());
            self.applied_permission = permission.clone();

            let mut events = vec![
                Event::Ready(ThreadSettings {
                    model: Some(model.clone()),
                    approval: permission,
                    ..ThreadSettings::default()
                }),
                Event::Models(parse_models(&response["response"]["models"], Some(&model))),
            ];

            if let Some((commands, structured)) = initialize_command_catalog(&response["response"])
            {
                self.structured_commands_published = structured;
                events.push(Event::Commands(commands));
            }

            return events;
        }

        Vec::new()
    }

    fn context_window_usage(&self) -> Option<ContextWindowUsage> {
        context_window_usage(
            self.context_usage,
            self.last_turn_usage,
            self.context_window,
        )
    }
}

#[cfg(test)]
mod tests;
