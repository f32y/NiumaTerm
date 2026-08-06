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
use std::os::windows::process::CommandExt as _;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::{fs, thread};

use serde_json::{Value, json};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::chat::{
    ContextWindowUsage, Event, Item, ModelInfo, SendOutcome, SlashCommandArguments,
    SlashCommandInfo, SlashCommandOutcome, SlashCommandRunPolicy, SlashCommandSource,
    ThreadSettings,
};
use crate::hook_store::home_dir;

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
    /// A structured initialize catalog carries richer metadata than the
    /// string-only first-turn fallback and must remain authoritative.
    structured_commands_published: bool,
    /// The most recent assistant message's input/output accounting represents
    /// the live context, unlike result-level totals which may sum retries and
    /// tool-loop iterations.
    context_input_tokens: u64,
    context_output_tokens: u64,
    context_window: Option<u64>,
}

impl Drop for Session {
    fn drop(&mut self) {
        // The npm `claude.cmd` shim starts a descendant process; killing only
        // cmd.exe would strand it. Closing stdin delivers EOF, which ends the
        // stream-json input and lets the CLI exit; the kill is belt-and-braces
        // cleanup for the shim itself.
        drop(self.stdin.take());
        let _ = self.child.kill();
    }
}

impl Session {
    /// Commands implemented by the Claude CLI but not necessarily included
    /// in every version's dynamic discovery payload.
    pub fn adapter_commands() -> Vec<SlashCommandInfo> {
        vec![SlashCommandInfo {
            name: "compact".to_string(),
            description: "Compact the current conversation context".to_string(),
            argument_hint: None,
            source: SlashCommandSource::Adapter,
            arguments: SlashCommandArguments::None,
            run_policy: SlashCommandRunPolicy::QueueUntilIdle,
        }]
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
        cwd: Option<String>,
        resume: Option<String>,
        deliver: impl Fn(Value) + Send + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        let mut command = Command::new("cmd.exe");

        command
            .args([
                "/D",
                "/C",
                "claude",
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
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW);

        if let Some(session_id) = &resume {
            command.args(["--resume", session_id]);
        }

        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }

        let mut child = command
            .spawn()
            .map_err(|err| format!("could not run `claude`: {err}"))?;

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
            stdin: Some(stdin),
            next_request_id: 1,
            ready: false,
            session_id: None,
            turn_active: false,
            turn_reported: false,
            pending_approval: None,
            applied_model: None,
            applied_permission: None,
            open_blocks: HashMap::new(),
            open_texts: VecDeque::new(),
            open_thinkings: VecDeque::new(),
            pending_tools: HashMap::new(),
            item_seq: 0,
            active_slash_command: None,
            structured_commands_published: false,
            context_input_tokens: 0,
            context_output_tokens: 0,
            context_window: None,
        };

        session.send(json!({
            "type": "control_request",
            "request_id": INIT_REQUEST_ID,
            "request": {"subtype": "initialize"},
        }));

        Ok(session)
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

    /// Interrupt the running turn (the Esc/Ctrl-C equivalent).
    pub fn interrupt(&mut self) {
        self.send_control(json!({"subtype": "interrupt"}));
    }

    /// The CLI's session id, once the `init` message delivered it. This is
    /// what `spawn`'s `resume` takes to reopen the conversation later.
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

    fn send_control(&mut self, request: Value) {
        let request_id = format!("nmt-{}", self.next_request_id);

        self.next_request_id += 1;
        self.send(json!({
            "type": "control_request",
            "request_id": request_id,
            "request": request,
        }));
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
        // Every subtype but `init` (status, hook_*, thinking_tokens, …) is
        // telemetry the UI ignores.
        if message["subtype"].as_str() != Some("init") {
            return Vec::new();
        }

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

            let failed = block["is_error"].as_bool().unwrap_or(false);
            let status = Some(if failed { "failed" } else { "completed" }.to_string());
            let output = tool_result_text(&block["content"]);

            let completed = match started {
                Item::CommandExecution { id, command, .. } => Item::CommandExecution {
                    id,
                    command,
                    aggregated_output: Some(output),
                    status,
                    exit_code: None,
                },
                Item::FileChange {
                    id, paths, diff, ..
                } => Item::FileChange {
                    id,
                    paths,
                    diff,
                    status,
                },
                Item::Other {
                    id,
                    kind,
                    title,
                    output: seeded,
                    ..
                } => Item::Other {
                    id,
                    kind,
                    title,
                    // Input-seeded detail (todo lists, plans) beats the
                    // result ack; everything else shows what the tool
                    // returned.
                    output: seeded.or(Some(output)),
                    status,
                },
                other => other,
            };

            events.push(Event::ItemCompleted(completed));
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
        // session starts on the catalog's "default" entry (spawn passes no
        // --model).
        if response["request_id"].as_str() == Some(INIT_REQUEST_ID) && !self.ready {
            let permission =
                Some(configured_permission_mode().unwrap_or_else(|| "default".to_string()));

            self.ready = true;
            self.applied_model = Some("default".to_string());
            self.applied_permission = permission.clone();

            let mut events = vec![
                Event::Ready(ThreadSettings {
                    model: Some("default".to_string()),
                    approval: permission,
                    ..ThreadSettings::default()
                }),
                Event::Models(parse_models(&response["response"]["models"])),
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

/// Map a tool-use block to a transcript item: Bash becomes a command card,
/// file-editing tools become file-change cards, everything else a titled tool
/// card.
fn tool_item(id: &str, name: &str, input: &Value) -> Item {
    let id = id.to_string();
    let status = Some("inProgress".to_string());

    match name {
        "Bash" => Item::CommandExecution {
            id,
            command: input["command"].as_str().unwrap_or_default().to_string(),
            aggregated_output: None,
            status,
            exit_code: None,
        },
        "Edit" | "Write" | "NotebookEdit" => Item::FileChange {
            id,
            paths: input["file_path"]
                .as_str()
                .unwrap_or("(unknown file)")
                .to_string(),
            diff: edit_diff(name, input),
            status,
        },
        _ => Item::Other {
            id,
            kind: name.to_string(),
            title: tool_title(input),
            output: input_detail(name, input),
            status,
        },
    }
}

/// Detail seeded from the tool INPUT, for tools whose interesting payload is
/// the request rather than the result (the result is just an ack). A seeded
/// detail survives completion; tools without one get the tool_result text.
fn input_detail(name: &str, input: &Value) -> Option<String> {
    match name {
        "TodoWrite" => input["todos"].as_array().map(|todos| {
            todos
                .iter()
                .filter_map(|todo| {
                    let content = todo["content"].as_str()?;
                    let mark = if todo["status"].as_str() == Some("completed") {
                        "x"
                    } else {
                        " "
                    };
                    Some(format!("- [{mark}] {content}"))
                })
                .collect::<Vec<_>>()
                .join("\n")
        }),
        "ExitPlanMode" => input["plan"].as_str().map(str::to_owned),
        _ => None,
    }
}

/// Reconstruct a reviewable +/- diff body from a file-editing tool's input.
/// The stream carries only the tool input (old/new text), so the change
/// itself is the reviewable content; per-line prefixes make it read (and
/// highlight) as a unified diff body.
fn edit_diff(name: &str, input: &Value) -> Option<String> {
    let (removed, added) = match name {
        "Edit" => (
            input["old_string"].as_str().unwrap_or_default(),
            input["new_string"].as_str().unwrap_or_default(),
        ),
        "Write" => ("", input["content"].as_str().unwrap_or_default()),
        "NotebookEdit" => ("", input["new_source"].as_str().unwrap_or_default()),
        _ => return None,
    };

    if removed.is_empty() && added.is_empty() {
        return None;
    }

    let mut diff = String::new();
    for line in removed.lines() {
        diff.push('-');
        diff.push_str(line);
        diff.push('\n');
    }
    for line in added.lines() {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    Some(diff)
}

/// Best-effort one-line label for an arbitrary tool call, from the input
/// fields common across built-in and MCP tools.
fn tool_title(input: &Value) -> String {
    for key in [
        "description",
        "file_path",
        "pattern",
        "query",
        "url",
        "path",
        "prompt",
        "skill",
    ] {
        if let Some(value) = input[key].as_str().filter(|s| !s.is_empty()) {
            return value.to_string();
        }
    }

    String::new()
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
fn parse_models(models: &Value) -> Vec<ModelInfo> {
    models
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
        .unwrap_or_default()
}

/// Extract readable text from a `tool_result` content payload, which is either
/// a plain string or an array of content blocks.
fn tool_result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn initialize_model_catalog_maps_value_and_display_name() {
        let models = json!([
            {"value": "default", "displayName": "Default (recommended)", "description": "…",
             "supportedEffortLevels": ["low", "high"]},
            {"value": "opus[1m]", "displayName": "Opus with 1M context"},
            {"displayName": "no value — skipped"}
        ]);

        let parsed = parse_models(&models);

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].model, "default");
        assert_eq!(parsed[0].display, "Default (recommended)");
        assert_eq!(parsed[0].efforts, vec!["low", "high"]);
        assert_eq!(parsed[1].model, "opus[1m]");
        assert!(parsed[1].efforts.is_empty());
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
