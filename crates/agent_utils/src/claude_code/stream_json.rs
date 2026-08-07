//! Claude Code stream-json chat session: process lifecycle, control-protocol
//! handshake, and translation of the wire protocol into typed events for a
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

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead as _, BufReader, Write as _};
use std::iter::once;
use std::mem::take;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};
use std::{fs, thread};

use serde_json::{Value, json};

use super::compaction::{compaction_metadata, parse_compaction};
use super::tool_items::{complete_tool_item, tool_item, tool_title};
#[cfg(test)]
use super::tool_items::{edit_diff, input_detail};
use crate::AgentLaunch;
use crate::chat::{
    ContextWindowUsage, Event, Item, ModelInfo, SendOutcome, SlashCommandArguments,
    SlashCommandInfo, SlashCommandOutcome, SlashCommandRunPolicy, SlashCommandSource,
    ThreadSettings,
};
use crate::hook_store::home_dir;
use crate::launcher::{ConfiguredLauncher, KillOnCloseJob};

/// Wire values for `--permission-mode` / the `set_permission_mode` control
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
const ANTHROPIC_MODEL_ENV: &str = "ANTHROPIC_MODEL";
const FILE_CHECKPOINTING_ENV: &str = "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingControlOperation {
    FileRewind,
}

fn launch_model(launch: &AgentLaunch) -> Option<String> {
    // Command environment overrides are last-value-wins, so the adapter must
    // resolve duplicate entries the same way as the spawned Claude process.
    launch
        .env
        .iter()
        .rev()
        .find(|(name, _)| name.trim().eq_ignore_ascii_case(ANTHROPIC_MODEL_ENV))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn initial_ready_model(model: Option<&str>) -> String {
    model.unwrap_or("default").to_string()
}

fn enable_file_checkpointing(command: &mut Command) {
    command.env(FILE_CHECKPOINTING_ENV, "true");
}

fn file_rewind_request(user_message_id: &str) -> Value {
    json!({
        "subtype": "rewind_files",
        "user_message_id": user_message_id,
    })
}

/// A `can_use_tool` control request awaiting the user's decision. The original
/// input is kept because an allow response must echo it as `updatedInput`, and
/// the CLI's permission suggestions back the "always allow" decision.
struct PendingApproval {
    request_id: String,
    input: Value,
    suggestions: Option<Value>,
}

pub struct Session {
    child: Child,
    process_job: Option<KillOnCloseJob>,
    stdin: Option<ChildStdin>,
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
    /// Last model/permission actually applied on the wire, so settings picked
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
    context_input_tokens: u64,
    context_output_tokens: u64,
    context_window: Option<u64>,
    /// A compaction is running. Tracked because the CLI re-announces it every
    /// 30 seconds while a long compaction proceeds, and the UI only needs the
    /// state transitions.
    compacting: bool,
}

impl Drop for Session {
    fn drop(&mut self) {
        // The npm `claude.cmd` shim starts a descendant process; killing only
        // cmd.exe would strand it. Closing stdin delivers EOF, which ends the
        // stream-json input and lets the CLI exit; the kill is belt-and-braces
        // cleanup for the shim itself.
        let _ = self.shutdown(Duration::from_millis(250), true);
    }
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
    /// listing for the same directory. Nothing is replayed on the wire — the
    /// UI pre-fills its transcript from the session file instead.
    pub fn spawn(
        launch: &AgentLaunch,
        cwd: Option<String>,
        resume: Option<String>,
        deliver: impl Fn(Value) + Send + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        let initial_model = launch_model(launch);
        let launcher = ConfiguredLauncher::from_launch(launch, "claude");
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
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(session_id) = &resume {
            command.args(["--resume", session_id]);
        }

        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }

        let mut child = command
            .spawn()
            .map_err(|err| format!("could not run `{executable}`: {err}"))?;
        let process_job = KillOnCloseJob::attach(&child).map_err(|error| {
            let _ = child.kill();
            let _ = child.wait();
            error
        })?;

        let stdin = child.stdin.take().ok_or("Claude stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("Claude stdout unavailable")?;
        let stderr = child.stderr.take().ok_or("Claude stderr unavailable")?;

        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(message) = serde_json::from_str::<Value>(&line) {
                    deliver(message);
                }
            }
        });

        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                on_stderr(line);
            }
        });

        let mut session = Self {
            child,
            process_job: Some(process_job),
            stdin: Some(stdin),
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
            context_input_tokens: 0,
            context_output_tokens: 0,
            context_window: None,
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
        drop(self.stdin.take());
        let started = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.process_job.take();
                    return Ok(());
                }
                Ok(None) if started.elapsed() < timeout => {
                    thread::sleep(Duration::from_millis(20));
                }
                Ok(None) if force => {
                    self.process_job.take();
                    self.child
                        .wait()
                        .map_err(|error| format!("could not wait for Claude to stop: {error}"))?;
                    return Ok(());
                }
                Ok(None) => return Err("Claude did not stop before the update timeout".to_string()),
                Err(error) => {
                    return Err(format!("could not observe Claude process exit: {error}"));
                }
            }
        }
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
        if self.stdin.is_none() {
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

            SendOutcome::StartedTurn
        }
    }

    /// Send a provider command through Claude's stream-json command path.
    /// This intentionally bypasses `send_user_message`: the UI must not add
    /// a user bubble or steer a running model turn for slash commands.
    pub fn execute_slash_command(&mut self, name: &str, arguments: &str) -> SlashCommandOutcome {
        if ui_owns_slash_command(name) {
            return SlashCommandOutcome::Rejected {
                message: "/rewind is handled by NiumaTerm's checkpoint picker.".to_string(),
            };
        }
        if !self.ready || self.stdin.is_none() {
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
        self.active_slash_command = Some(name.to_string());

        SlashCommandOutcome::Accepted
    }

    /// Restore files tracked by Claude to the state captured before the user
    /// message. Completion arrives asynchronously as `FileRewindCompleted`.
    pub fn rewind_files(&mut self, user_message_id: &str) -> SlashCommandOutcome {
        if !self.ready || self.stdin.is_none() {
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

    /// Write one line. Failures are not surfaced here: a dead process also
    /// closes its stdout, so the reader-side EOF is the single exit-detection
    /// path.
    fn send(&mut self, message: Value) {
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = writeln!(stdin, "{message}").and_then(|_| stdin.flush());
        }
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
        // (e.g. `claude-opus-5[1m]`), which is not a catalog wire value, so
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
            self.context_input_tokens = post_tokens;
            self.context_output_tokens = 0;

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

                let usage = &event["message"]["usage"];
                self.context_input_tokens = claude_input_tokens(usage);
                self.context_output_tokens = usage["output_tokens"].as_u64().unwrap_or(0);

                self.context_window_usage()
                    .map(Event::ContextWindowUpdated)
                    .into_iter()
                    .collect()
            }
            Some("message_delta") => {
                if let Some(output_tokens) = event["usage"]["output_tokens"].as_u64() {
                    self.context_output_tokens = output_tokens;
                }

                self.context_window_usage()
                    .map(Event::ContextWindowUpdated)
                    .into_iter()
                    .collect()
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

        if let Some(usage) = self.context_window_usage() {
            events.push(Event::ContextWindowUpdated(usage));
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
        let used_tokens = self
            .context_input_tokens
            .saturating_add(self.context_output_tokens);

        (used_tokens > 0).then_some(ContextWindowUsage {
            used_tokens,
            max_tokens: self.context_window,
        })
    }
}

fn resolve_pending_control_operation(
    pending: &mut HashMap<String, PendingControlOperation>,
    response: &Value,
) -> Option<Event> {
    let request_id = response["request_id"].as_str()?;
    let operation = pending.remove(request_id)?;
    let error = match response["subtype"].as_str() {
        Some("success") => None,
        Some("error") => Some(
            response["error"]
                .as_str()
                .unwrap_or("unknown Claude control error")
                .to_string(),
        ),
        _ => Some("Claude returned a malformed file restore response.".to_string()),
    };

    match operation {
        PendingControlOperation::FileRewind => Some(Event::FileRewindCompleted { error }),
    }
}

fn fail_pending_control_operations(
    pending: &mut HashMap<String, PendingControlOperation>,
    message: &str,
) -> Vec<Event> {
    let operations = take(pending);

    operations
        .into_values()
        .map(|operation| match operation {
            PendingControlOperation::FileRewind => Event::FileRewindCompleted {
                error: Some(message.to_string()),
            },
        })
        .collect()
}

fn claude_input_tokens(usage: &Value) -> u64 {
    [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]
    .into_iter()
    .map(|field| usage[field].as_u64().unwrap_or(0))
    .sum()
}

/// Translate a `system/status` message into compaction progress events.
///
/// The subtype multiplexes unrelated notifications — a per-request `requesting`
/// marker, permission-mode echoes, and compaction — so each transition is
/// recognized by its own field rather than by `status` alone. `compact_result`
/// appears only on a compaction's final message, and `requesting` also fires for
/// the summarization call that compaction itself makes, which would otherwise
/// look like the end of it. `compacting` is re-announced roughly every 30
/// seconds while a long compaction runs, so `active` suppresses the repeats.
fn compaction_progress(active: &mut bool, message: &Value) -> Vec<Event> {
    if let Some(result) = message["compact_result"].as_str() {
        *active = false;

        return vec![Event::CompactionFinished {
            error: (result != "success").then(|| {
                message["compact_error"]
                    .as_str()
                    .unwrap_or("Compacting the conversation failed.")
                    .to_string()
            }),
        }];
    }

    if message["status"].as_str() == Some("compacting") && !*active {
        *active = true;

        return vec![Event::CompactionStarted];
    }

    Vec::new()
}

fn claude_context_window(model_usage: &Value) -> Option<u64> {
    model_usage
        .as_object()?
        .values()
        .filter_map(|usage| {
            usage["contextWindow"]
                .as_u64()
                .or_else(|| usage["context_window"].as_u64())
                .filter(|value| *value > 0)
        })
        .max()
}

fn slash_command_text(name: &str, arguments: &str) -> String {
    let name = name.trim().trim_start_matches('/');
    let arguments = arguments.trim();

    if arguments.is_empty() {
        format!("/{name}")
    } else {
        format!("/{name} {arguments}")
    }
}

fn ui_owns_slash_command(name: &str) -> bool {
    name.trim()
        .trim_start_matches('/')
        .eq_ignore_ascii_case("rewind")
}

fn claude_result_error(message: &Value) -> Option<String> {
    if !message["is_error"].as_bool().unwrap_or(false)
        && message["subtype"].as_str() == Some("success")
    {
        return None;
    }

    // Startup failures (e.g. a `--resume` id whose transcript is gone) put
    // their reason in `errors`, not `result`.
    Some(
        message["result"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| message["errors"][0].as_str().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| message["subtype"].as_str().unwrap_or("turn failed"))
            .to_string(),
    )
}

/// Prefer the current structured initialize field while accepting the older
/// control-response spelling. The boolean tells first-turn handling whether
/// a later string-only catalog is allowed to replace this result.
fn initialize_command_catalog(response: &Value) -> Option<(Vec<SlashCommandInfo>, bool)> {
    if !response["commands"].is_null() {
        Some((parse_slash_commands(&response["commands"]), true))
    } else if !response["slash_commands"].is_null() {
        Some((parse_slash_commands(&response["slash_commands"]), false))
    } else {
        None
    }
}

fn legacy_command_catalog(
    structured_commands_published: bool,
    commands: &Value,
) -> Option<Vec<SlashCommandInfo>> {
    (!structured_commands_published).then(|| parse_slash_commands(commands))
}

/// Claude versions have emitted both string entries and richer objects. The
/// parser accepts both and expands aliases while enforcing the single-token
/// names the composer can address. A new event is a complete replacement.
fn parse_slash_commands(commands: &Value) -> Vec<SlashCommandInfo> {
    let mut seen = HashSet::new();
    let mut parsed = Vec::new();

    for entry in commands.as_array().into_iter().flatten() {
        let Some(raw_name) = entry
            .as_str()
            .or_else(|| entry["name"].as_str())
            .or_else(|| entry["command"].as_str())
        else {
            continue;
        };
        let canonical = raw_name.trim().trim_start_matches('/').to_ascii_lowercase();
        let argument_hint = entry["argumentHint"]
            .as_str()
            .or_else(|| entry["argument_hint"].as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let description = entry["description"]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Run Claude's /{canonical} command"));
        let aliases = entry["aliases"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str);

        for raw_name in once(raw_name).chain(aliases) {
            let name = raw_name.trim().trim_start_matches('/').to_ascii_lowercase();

            if name.is_empty()
                || name.chars().any(char::is_whitespace)
                || !seen.insert(name.clone())
            {
                continue;
            }

            parsed.push(SlashCommandInfo {
                name,
                description: description.clone(),
                arguments: if argument_hint.is_some() {
                    SlashCommandArguments::Freeform
                } else {
                    SlashCommandArguments::None
                },
                argument_hint: argument_hint.clone(),
                source: SlashCommandSource::Provider,
                run_policy: SlashCommandRunPolicy::QueueUntilIdle,
            });
        }
    }

    parsed
}

/// The permission mode the CLI will start in, from `~/.claude/settings.json`
/// (`permissions.defaultMode`). The protocol has no way to query the mode
/// before the first turn, so this mirrors the CLI's own config resolution;
/// project-level overrides are not consulted (rare, and the first turn's
/// `init` message corrects any mismatch).
fn configured_permission_mode() -> Option<String> {
    let path = home_dir()?.join(".claude").join("settings.json");
    let settings: Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;

    settings["permissions"]["defaultMode"]
        .as_str()
        .map(str::to_owned)
}

/// Human-readable summary of a `can_use_tool` request for the approval card.
fn approval_description(tool_name: &str, input: &Value) -> String {
    match tool_name {
        "Bash" => format!(
            "Run command: `{}`",
            input["command"].as_str().unwrap_or_default()
        ),
        "Edit" | "Write" | "NotebookEdit" => format!(
            "Edit file: {}",
            input["file_path"].as_str().unwrap_or("(unknown file)")
        ),
        "ExitPlanMode" => format!(
            "Approve Claude's plan:\n\n{}",
            input["plan"].as_str().unwrap_or_default()
        ),
        _ => {
            let detail = tool_title(input);

            if detail.is_empty() {
                tool_name.to_string()
            } else {
                format!("{tool_name}: {detail}")
            }
        }
    }
}

/// The model catalog from the initialize response: `value` is the wire name
/// (`"default"`, `"opus[1m]"`, …), `displayName` the menu label. Claude has
/// no per-model service tiers, but each entry lists its reasoning-effort
/// levels in `supportedEffortLevels` (absent on models without effort, e.g.
/// Haiku).
fn parse_models(models: &Value, selected_model: Option<&str>) -> Vec<ModelInfo> {
    let mut parsed: Vec<ModelInfo> = models
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|entry| {
                    let model = entry["value"].as_str()?.to_string();
                    let display = entry["displayName"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&model)
                        .to_string();
                    let efforts = entry["supportedEffortLevels"]
                        .as_array()
                        .map(|levels| {
                            levels
                                .iter()
                                .filter_map(|v| v.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default();

                    Some(ModelInfo {
                        model,
                        display,
                        tiers: Vec::new(),
                        default_tier: None,
                        efforts,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    if let Some(model) = selected_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        && !parsed.iter().any(|entry| entry.model == model)
    {
        parsed.insert(
            0,
            ModelInfo {
                model: model.to_string(),
                display: model.to_string(),
                tiers: Vec::new(),
                default_tier: None,
                efforts: Vec::new(),
            },
        );
    }

    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_claude_process_enables_sdk_file_checkpointing() {
        let mut command = Command::new("claude");
        command.env(FILE_CHECKPOINTING_ENV, "false");

        enable_file_checkpointing(&mut command);

        let value = command
            .get_envs()
            .find(|(name, _)| *name == FILE_CHECKPOINTING_ENV)
            .and_then(|(_, value)| value)
            .and_then(|value| value.to_str());
        assert_eq!(value, Some("true"));
    }

    #[test]
    fn rewind_is_an_idle_ui_command_not_a_provider_slash_turn() {
        let commands = Session::adapter_commands();
        let rewind = commands
            .iter()
            .find(|command| command.name == "rewind")
            .expect("Claude rewind metadata");

        assert_eq!(rewind.source, SlashCommandSource::Adapter);
        assert_eq!(rewind.arguments, SlashCommandArguments::None);
        assert_eq!(rewind.run_policy, SlashCommandRunPolicy::IdleOnly);
        assert!(ui_owns_slash_command("rewind"));
        assert!(ui_owns_slash_command("/ReWiNd"));
        assert!(!ui_owns_slash_command("compact"));
    }

    #[cfg(windows)]
    #[test]
    fn fake_stream_json_process_never_receives_rewind_as_a_user_turn() {
        use std::env;
        use std::path::Path;
        use std::sync::mpsc;
        use std::time::Duration;

        use uuid::Uuid;

        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude/fake-stream-json.cmd");
        let log = env::temp_dir().join(format!("niumaterm-fake-claude-{}.jsonl", Uuid::new_v4()));
        let launch = AgentLaunch {
            executable: fixture.to_string_lossy().into_owned(),
            env: vec![(
                "NMT_FAKE_STREAM_LOG".to_string(),
                log.to_string_lossy().into_owned(),
            )],
            ..AgentLaunch::default()
        };
        let (messages_tx, messages_rx) = mpsc::channel();
        let mut session = Session::spawn(
            &launch,
            None,
            None,
            move |message| {
                let _ = messages_tx.send(message);
            },
            |_| {},
        )
        .expect("fake Claude process starts");
        let init = messages_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("fake Claude init");
        assert!(
            session
                .process(init)
                .iter()
                .any(|event| matches!(event, Event::Ready(_)))
        );

        assert!(matches!(
            session.execute_slash_command("rewind", ""),
            SlashCommandOutcome::Rejected { .. }
        ));

        for _ in 0..50 {
            if log.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        drop(session);
        let input = fs::read_to_string(&log).expect("fake process captured stdin");
        assert!(input.contains("initialize"));
        assert!(!input.contains("/rewind"));
        assert!(!input.contains("\"type\":\"user\""));
        fs::remove_file(log).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn resumed_session_id_is_available_before_the_first_init_event() {
        use std::env;
        use std::path::Path;
        use std::time::Duration;

        use uuid::Uuid;

        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude/fake-stream-json.cmd");
        let log = env::temp_dir().join(format!("niumaterm-resume-{}.jsonl", Uuid::new_v4()));
        let launch = AgentLaunch {
            executable: fixture.to_string_lossy().into_owned(),
            env: vec![(
                "NMT_FAKE_STREAM_LOG".to_string(),
                log.to_string_lossy().into_owned(),
            )],
            ..AgentLaunch::default()
        };
        let resume_id = "70000000-0000-4000-8000-000000000000".to_string();
        let session = Session::spawn(&launch, None, Some(resume_id.clone()), |_| {}, |_| {})
            .expect("fake resumed Claude process starts");

        let published_id = session.session_id().map(str::to_owned);
        drop(session);
        for _ in 0..50 {
            if log.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        if log.exists() {
            fs::remove_file(log).unwrap();
        }

        assert_eq!(published_id, Some(resume_id));
    }

    #[test]
    fn file_rewind_request_matches_the_sdk_control_shape() {
        assert_eq!(
            file_rewind_request("user-message-1"),
            json!({
                "subtype": "rewind_files",
                "user_message_id": "user-message-1",
            })
        );
    }

    #[test]
    fn file_rewind_control_response_is_correlated_by_request_id() {
        let mut pending =
            HashMap::from([("nmt-7".to_string(), PendingControlOperation::FileRewind)]);

        assert_eq!(
            resolve_pending_control_operation(
                &mut pending,
                &json!({"request_id": "other", "subtype": "success"})
            ),
            None
        );
        assert!(pending.contains_key("nmt-7"));
        assert_eq!(
            resolve_pending_control_operation(
                &mut pending,
                &json!({"request_id": "nmt-7", "subtype": "success"})
            ),
            Some(Event::FileRewindCompleted { error: None })
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn file_rewind_rejection_and_malformed_responses_are_nonfatal_results() {
        for (subtype, expected) in [
            ("error", "checkpoint expired"),
            (
                "unexpected",
                "Claude returned a malformed file restore response.",
            ),
        ] {
            let mut pending =
                HashMap::from([("nmt-8".to_string(), PendingControlOperation::FileRewind)]);
            let response = if subtype == "error" {
                json!({
                    "request_id": "nmt-8",
                    "subtype": subtype,
                    "error": "checkpoint expired",
                })
            } else {
                json!({"request_id": "nmt-8", "subtype": subtype})
            };

            assert_eq!(
                resolve_pending_control_operation(&mut pending, &response),
                Some(Event::FileRewindCompleted {
                    error: Some(expected.to_string()),
                })
            );
            assert!(pending.is_empty());
        }
    }

    #[test]
    fn process_exit_fails_and_clears_pending_file_rewinds() {
        let mut pending =
            HashMap::from([("nmt-9".to_string(), PendingControlOperation::FileRewind)]);

        assert_eq!(
            fail_pending_control_operations(&mut pending, "Claude exited."),
            vec![Event::FileRewindCompleted {
                error: Some("Claude exited.".into()),
            }]
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn content_bearing_inputs_seed_the_card_detail() {
        let todos = input_detail(
            "TodoWrite",
            &json!({"todos": [
                {"content": "done thing", "status": "completed"},
                {"content": "next thing", "status": "pending"},
            ]}),
        );
        assert_eq!(todos.as_deref(), Some("- [x] done thing\n- [ ] next thing"));

        let plan = input_detail("ExitPlanMode", &json!({"plan": "1. do it"}));
        assert_eq!(plan.as_deref(), Some("1. do it"));

        assert_eq!(input_detail("Grep", &json!({"pattern": "x"})), None);
    }

    #[test]
    fn edit_diff_prefixes_old_and_new_lines() {
        let diff = edit_diff("Edit", &json!({"old_string": "a\nb", "new_string": "c"}));
        assert_eq!(diff.as_deref(), Some("-a\n-b\n+c\n"));

        assert_eq!(edit_diff("Edit", &json!({})), None);
    }

    #[test]
    fn bash_and_file_tools_map_to_dedicated_cards() {
        let bash = tool_item("t1", "Bash", &json!({"command": "cargo check"}));

        assert_eq!(
            bash,
            Item::CommandExecution {
                id: "t1".into(),
                command: "cargo check".into(),
                aggregated_output: None,
                status: Some("inProgress".into()),
                exit_code: None,
            }
        );

        let write = tool_item(
            "t2",
            "Write",
            &json!({"file_path": "C:\\a.txt", "content": "x"}),
        );

        assert_eq!(
            write,
            Item::FileChange {
                id: "t2".into(),
                paths: "C:\\a.txt".into(),
                diff: Some("+x\n".into()),
                status: Some("inProgress".into()),
            }
        );

        let grep = tool_item("t3", "Grep", &json!({"pattern": "foo.*bar"}));

        assert_eq!(
            grep,
            Item::Other {
                id: "t3".into(),
                kind: "Grep".into(),
                title: "foo.*bar".into(),
                output: None,
                status: Some("inProgress".into()),
            }
        );
    }

    #[test]
    fn only_the_compaction_status_shapes_drive_progress_events() {
        let mut active = false;

        // Per-request and permission-mode notifications share the subtype.
        assert!(
            compaction_progress(&mut active, &json!({"status": "requesting"})).is_empty(),
            "an API request start is not compaction"
        );
        assert!(
            compaction_progress(
                &mut active,
                &json!({"status": null, "permissionMode": "acceptEdits"})
            )
            .is_empty(),
            "a permission-mode echo must not end a compaction"
        );
        assert!(!active);

        assert_eq!(
            compaction_progress(&mut active, &json!({"status": "compacting"})),
            vec![Event::CompactionStarted]
        );
        assert!(active);

        // The CLI re-announces the running compaction; the UI needs one edge.
        assert!(
            compaction_progress(&mut active, &json!({"status": "compacting"})).is_empty(),
            "repeat announcements are not new transitions"
        );
        // The summarization call itself reports as a request in flight.
        assert!(compaction_progress(&mut active, &json!({"status": "requesting"})).is_empty());
        assert!(active, "a request in flight must not end the compaction");

        assert_eq!(
            compaction_progress(
                &mut active,
                &json!({"status": null, "compact_result": "success"})
            ),
            vec![Event::CompactionFinished { error: None }]
        );
        assert!(!active);
    }

    #[test]
    fn a_failed_compaction_reports_its_reason() {
        let mut active = true;

        assert_eq!(
            compaction_progress(
                &mut active,
                &json!({"status": null, "compact_result": "failed",
                    "compact_error": "not enough messages to summarize"})
            ),
            vec![Event::CompactionFinished {
                error: Some("not enough messages to summarize".into())
            }]
        );
        assert!(!active);

        // A failure with no detail still has to surface as a failure.
        let mut active = true;
        let events = compaction_progress(&mut active, &json!({"compact_result": "failed"}));

        assert!(matches!(
            events.as_slice(),
            [Event::CompactionFinished { error: Some(_) }]
        ));
    }

    #[test]
    fn initialize_model_catalog_maps_value_and_display_name() {
        let models = json!([
            {"value": "default", "displayName": "Default (recommended)", "description": "…",
             "supportedEffortLevels": ["low", "high"]},
            {"value": "opus[1m]", "displayName": "Opus with 1M context"},
            {"displayName": "no value — skipped"}
        ]);

        let parsed = parse_models(&models, None);

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].model, "default");
        assert_eq!(parsed[0].display, "Default (recommended)");
        assert_eq!(parsed[0].efforts, vec!["low", "high"]);
        assert_eq!(parsed[1].model, "opus[1m]");
        assert!(parsed[1].efforts.is_empty());
    }

    #[test]
    fn initialize_model_catalog_keeps_a_selected_custom_model() {
        let parsed = parse_models(
            &json!([{"value": "default", "displayName": "Default"}]),
            Some("claude-custom-model"),
        );

        assert_eq!(parsed[0].model, "claude-custom-model");
        assert_eq!(parsed[1].model, "default");
    }

    #[test]
    fn initialize_uses_model_pinned_by_launch_environment() {
        let launch = AgentLaunch {
            executable: "claude".into(),
            env: vec![
                ("UNRELATED".into(), "value".into()),
                (
                    ANTHROPIC_MODEL_ENV.into(),
                    "claude-opus-4-8-v4-flash[1m]".into(),
                ),
            ],
            ..AgentLaunch::default()
        };

        let model = initial_ready_model(launch_model(&launch).as_deref());

        assert_eq!(model, "claude-opus-4-8-v4-flash[1m]");
    }

    #[test]
    fn approval_descriptions_name_the_action() {
        assert_eq!(
            approval_description("Bash", &json!({"command": "rm -rf build"})),
            "Run command: `rm -rf build`"
        );
        assert_eq!(
            approval_description("Write", &json!({"file_path": "a.txt"})),
            "Edit file: a.txt"
        );
        assert_eq!(
            approval_description("mcp__github__search", &json!({"query": "is:open"})),
            "mcp__github__search: is:open"
        );
    }

    #[test]
    fn dynamic_commands_accept_both_wire_shapes_and_drop_invalid_duplicates() {
        let parsed = parse_slash_commands(&json!([
            "/Review",
            {"name": "compact", "description": "Compact it", "argumentHint": "[focus]",
             "aliases": ["summarize", "/shrink", "not valid"]},
            {"command": "/review"},
            "",
            "not valid"
        ]));

        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[0].name, "review");
        assert_eq!(parsed[1].name, "compact");
        assert_eq!(parsed[2].name, "summarize");
        assert_eq!(parsed[3].name, "shrink");
        assert_eq!(parsed[1].argument_hint.as_deref(), Some("[focus]"));
        assert_eq!(parsed[2].description, "Compact it");
        assert_eq!(parsed[1].arguments, SlashCommandArguments::Freeform);
        assert!(parse_slash_commands(&Value::Null).is_empty());
    }

    #[test]
    fn initialize_commands_are_primary_and_legacy_catalogs_are_fallbacks() {
        let response = json!({
            "commands": [{"name": "plugin:review", "aliases": ["pr"]}],
            "slash_commands": ["legacy"]
        });
        let (commands, structured) = initialize_command_catalog(&response).unwrap();

        assert!(structured);
        assert_eq!(
            commands
                .iter()
                .map(|command| command.name.as_str())
                .collect::<Vec<_>>(),
            vec!["plugin:review", "pr"]
        );
        assert!(legacy_command_catalog(structured, &json!(["legacy"])).is_none());

        let (legacy, structured) =
            initialize_command_catalog(&json!({"slash_commands": ["legacy"]})).unwrap();
        assert!(!structured);
        assert_eq!(legacy[0].name, "legacy");
        assert_eq!(
            legacy_command_catalog(structured, &json!(["newer"])).unwrap()[0].name,
            "newer"
        );
        assert!(initialize_command_catalog(&json!({})).is_none());
    }

    #[test]
    fn provider_command_text_is_not_an_ordinary_prompt_shape() {
        assert_eq!(slash_command_text("/compact", ""), "/compact");
        assert_eq!(
            slash_command_text("review", "  focus here  "),
            "/review focus here"
        );
        assert_eq!(
            claude_result_error(&json!({
                "type": "result",
                "subtype": "error_during_execution",
                "is_error": true,
                "result": "provider rejected command"
            })),
            Some("provider rejected command".into())
        );
        assert_eq!(
            claude_result_error(&json!({"subtype": "success", "is_error": false})),
            None
        );
    }
}
