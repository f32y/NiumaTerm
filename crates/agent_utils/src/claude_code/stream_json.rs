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

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::windows::process::CommandExt as _;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread;

use serde_json::{Value, json};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::chat::{Event, Item, ModelInfo, SendOutcome, ThreadSettings};

/// Wire values for `--permission-mode` / the `set_permission_mode` control
/// request.
pub const PERMISSION_OPTIONS: [&str; 4] = ["default", "acceptEdits", "plan", "bypassPermissions"];

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
    /// Spawn `claude` in bidirectional stream-json mode and send the SDK-style
    /// `initialize` control request. Every parsed stdout line is handed to
    /// `deliver` (from a reader thread — hop threads before calling
    /// [`Session::process`]); stderr lines go to `on_stderr`.
    pub fn spawn(
        cwd: Option<String>,
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

    /// Interrupt the running turn (the Esc/Ctrl-C equivalent).
    pub fn interrupt(&mut self) {
        self.send_control(json!({"subtype": "interrupt"}));
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
        // telemetry the UI ignores. The initialize handshake normally seeds
        // the pickers before the first turn; `init` — emitted when the first
        // turn opens — is the fallback for a CLI that didn't answer it, and
        // is otherwise skipped so it can't clobber the user's picks.
        if message["subtype"].as_str() != Some("init") || self.ready {
            return Vec::new();
        }
        self.ready = true;

        let model = message["model"].as_str().map(str::to_owned);
        let permission = message["permissionMode"].as_str().map(str::to_owned);

        self.applied_model = model.clone();
        self.applied_permission = permission.clone();

        vec![Event::Ready(ThreadSettings {
            model,
            approval: permission,
            ..ThreadSettings::default()
        })]
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

                Vec::new()
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
                Item::FileChange { id, paths, .. } => Item::FileChange { id, paths, status },
                Item::Other {
                    id, kind, title, ..
                } => Item::Other {
                    id,
                    kind,
                    title,
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

        let error = if message["is_error"].as_bool().unwrap_or(false)
            || message["subtype"].as_str() != Some("success")
        {
            Some(
                message["result"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| message["subtype"].as_str().unwrap_or("turn failed"))
                    .to_string(),
            )
        } else {
            None
        };

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
        // model catalog, so the pickers show real values immediately. The
        // session starts on the catalog's "default" entry and the default
        // permission mode (spawn passes no --permission-mode).
        if response["request_id"].as_str() == Some(INIT_REQUEST_ID) && !self.ready {
            self.ready = true;
            self.applied_model = Some("default".to_string());
            self.applied_permission = Some("default".to_string());

            return vec![
                Event::Ready(ThreadSettings {
                    model: Some("default".to_string()),
                    approval: Some("default".to_string()),
                    ..ThreadSettings::default()
                }),
                Event::Models(parse_models(&response["response"]["models"])),
            ];
        }

        Vec::new()
    }
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
            status,
        },
        _ => Item::Other {
            id,
            kind: name.to_string(),
            title: tool_title(input),
            status,
        },
    }
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
}
