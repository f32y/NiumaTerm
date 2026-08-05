//! Codex `app-server` chat session: process lifecycle, JSON-RPC handshake,
//! and translation of the wire protocol into typed events for a chat UI.
//!
//! The app-server protocol is Codex's supported integration surface for
//! third-party UIs (it powers the VS Code extension). One `Session` owns one
//! `codex app-server` process and one conversation thread on it.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::windows::process::CommandExt as _;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Value, json};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

pub use crate::chat::{
    Event, Item, ModelInfo, ReplayItem, SendOutcome, SessionSummary, SlashCommandArguments,
    SlashCommandInfo, SlashCommandOutcome, SlashCommandRunPolicy, SlashCommandSource,
    ThreadSettings,
};

/// JSON-RPC ids for the fixed handshake requests; turn requests count up from
/// `FIRST_TURN_RPC_ID` so response routing can tell the phases apart.
const INIT_RPC_ID: u64 = 1;
const THREAD_START_RPC_ID: u64 = 2;
const MODEL_LIST_RPC_ID: u64 = 3;
const THREAD_LIST_RPC_ID: u64 = 4;
const THREAD_RESUME_RPC_ID: u64 = 5;
const FIRST_TURN_RPC_ID: u64 = 100;

/// First page size for the history list; enough to fill the visible list
/// several times over. `nextCursor` remains available for deeper paging.
const THREAD_LIST_LIMIT: u64 = 50;

/// Wire values for approval-policy selection (`AskForApproval` serializes
/// kebab-case).
pub const APPROVAL_OPTIONS: [&str; 3] = ["untrusted", "on-request", "never"];
/// `(wire value, display label)` for sandbox selection (`SandboxPolicy` uses a
/// camelCase `type` tag).
pub const SANDBOX_OPTIONS: [(&str, &str); 3] = [
    ("readOnly", "read-only"),
    ("workspaceWrite", "workspace-write"),
    ("dangerFullAccess", "full-access"),
];
/// Wire values for reasoning effort (`ReasoningEffort` serializes lowercase).
pub const EFFORT_OPTIONS: [&str; 8] = [
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

pub struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
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
}

impl Drop for Session {
    fn drop(&mut self) {
        // The npm `codex.cmd` shim starts a descendant process; killing only
        // cmd.exe would strand it. Closing stdin delivers EOF, which
        // app-server treats as shutdown, and the reader thread exits with the
        // pipe. The kill is a belt-and-braces cleanup for the shim itself.
        drop(self.stdin.take());
        let _ = self.child.kill();
    }
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
        ]
    }

    /// Spawn `codex app-server` with piped stdio and send the `initialize`
    /// request. Every parsed stdout line is handed to `deliver` (from a
    /// reader thread — hop threads before calling [`Session::process`]);
    /// stderr lines go to `on_stderr`. Dropping `deliver` signals EOF: the
    /// closure is owned by the reader thread and dropped when the pipe closes.
    pub fn spawn(
        cwd: Option<String>,
        deliver: impl Fn(Value) + Send + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        let mut command = Command::new("cmd.exe");

        command
            .args(["/D", "/C", "codex", "app-server"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW);

        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }

        let mut child = command
            .spawn()
            .map_err(|err| format!("could not run `codex app-server`: {err}"))?;

        let stdin = child.stdin.take().ok_or("Codex stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("Codex stdout unavailable")?;
        let stderr = child.stderr.take().ok_or("Codex stderr unavailable")?;

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
            next_rpc_id: FIRST_TURN_RPC_ID,
            thread_id: None,
            current_turn: None,
            pending_approval: None,
            history_cursor: None,
            pending_commands: HashMap::new(),
        };

        session.send(json!({
            "jsonrpc": "2.0",
            "id": INIT_RPC_ID,
            "method": "initialize",
            "params": {"clientInfo": {"name": "NiumaTerm", "version": "0.1.0"}},
        }));

        Ok(session)
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
        let Some(thread_id) = self.thread_id.clone() else {
            return SendOutcome::NotReady;
        };

        let rpc_id = self.alloc_rpc_id();

        if let Some(turn_id) = self.current_turn.clone() {
            self.send(json!({
                "jsonrpc": "2.0",
                "id": rpc_id,
                "method": "turn/steer",
                "params": {
                    "threadId": thread_id,
                    "expectedTurnId": turn_id,
                    "input": [{"type": "text", "text": text}],
                },
            }));

            return SendOutcome::Steered;
        }

        let mut params = json!({
            "threadId": thread_id,
            "input": [{"type": "text", "text": text}],
        });

        if let Some(model) = &settings.model {
            params["model"] = json!(model);
        }
        if let Some(approval) = &settings.approval {
            params["approvalPolicy"] = json!(approval);
        }
        if let Some(sandbox) = &settings.sandbox {
            params["sandboxPolicy"] = json!({"type": sandbox});
        }
        if let Some(effort) = &settings.effort {
            params["effort"] = json!(effort);
        }
        // Always sent: an explicit null resets to the normal tier.
        params["serviceTier"] = json!(settings.tier);

        self.send(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "turn/start",
            "params": params,
        }));

        SendOutcome::StartedTurn
    }

    /// Execute the two Codex operations NiumaTerm can represent faithfully.
    /// Other Codex CLI surface areas (skills, plugins, configuration) are not
    /// advertised as chat commands until they have dedicated UI semantics.
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
        self.send(json!({
            "jsonrpc": "2.0",
            "id": THREAD_RESUME_RPC_ID,
            "method": "thread/resume",
            "params": {"threadId": thread_id},
        }));
    }

    /// Request the next history page; a no-op when the final page arrived.
    pub fn request_more_history(&mut self) {
        let Some(cursor) = self.history_cursor.take() else {
            return;
        };

        self.send(json!({
            "jsonrpc": "2.0",
            "id": THREAD_LIST_RPC_ID,
            "method": "thread/list",
            "params": {
                "cwd": ".",
                "sortKey": "recency_at",
                "limit": THREAD_LIST_LIMIT,
                "cursor": cursor,
            },
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

    /// Write one request line. Failures are not surfaced here: a dead process
    /// also closes its stdout, so the reader-side EOF is the single
    /// exit-detection path.
    fn send(&mut self, message: Value) {
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = writeln!(stdin, "{message}").and_then(|_| stdin.flush());
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
        let pending_command = self.pending_commands.remove(&rpc_id);
        let is_command = pending_command.is_some();

        if let Some(error) = message["error"]["message"].as_str() {
            if let Some(command) = pending_command.as_deref() {
                return vec![Event::SlashCommandResult {
                    name: command.to_string(),
                    outcome: codex_command_response(command, Some(error)),
                }];
            }

            // A failed resume (deleted/corrupt thread) is not fatal: the
            // session still has the thread it started with, so the composer
            // keeps working for a fresh conversation.
            let message = if rpc_id == THREAD_RESUME_RPC_ID {
                format!("Could not resume session: {error}")
            } else {
                error.to_string()
            };

            return vec![Event::Error {
                message,
                fatal: !is_command && rpc_id <= THREAD_START_RPC_ID,
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
                self.send(json!({
                    "jsonrpc": "2.0",
                    "id": THREAD_START_RPC_ID,
                    "method": "thread/start",
                    "params": {},
                }));

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
                self.send(json!({
                    "jsonrpc": "2.0",
                    "id": THREAD_LIST_RPC_ID,
                    "method": "thread/list",
                    "params": {
                        "cwd": ".",
                        "sortKey": "recency_at",
                        "limit": THREAD_LIST_LIMIT,
                    },
                }));

                vec![Event::Ready(parse_thread_settings(result))]
            }
            MODEL_LIST_RPC_ID => vec![Event::Models(parse_models(&message["result"]))],
            THREAD_LIST_RPC_ID => {
                let result = &message["result"];

                self.history_cursor = result["nextCursor"].as_str().map(str::to_owned);

                // The thread this session just started is part of the wire
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

                vec![
                    Event::Replay(parse_replay(&result["thread"]["turns"])),
                    // Resume restores the thread's persisted model/effort;
                    // Ready re-seeds the pickers with those values.
                    Event::Ready(parse_thread_settings(result)),
                ]
            }
            _ => Vec::new(),
        }
    }

    fn process_notification(&mut self, method: &str, params: &Value) -> Vec<Event> {
        match method {
            "turn/started" => {
                self.current_turn = params["turn"]["id"].as_str().map(str::to_owned);

                vec![Event::TurnStarted]
            }
            "turn/completed" => {
                self.current_turn = None;

                let error = (params["turn"]["status"].as_str() == Some("failed"))
                    .then(|| params["turn"]["error"]["message"].as_str())
                    .flatten()
                    .map(str::to_owned);

                vec![Event::TurnCompleted { error }]
            }
            "item/started" => parse_item(&params["item"])
                .map(Event::ItemStarted)
                .into_iter()
                .collect(),
            "item/completed" => parse_item(&params["item"])
                .map(Event::ItemCompleted)
                .into_iter()
                .collect(),
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

fn codex_command_request(rpc_id: u64, thread_id: &str, name: &str) -> Option<Value> {
    match name {
        "compact" => Some(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "thread/compact/start",
            "params": {"threadId": thread_id},
        })),
        "review" => Some(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "review/start",
            "params": {
                "threadId": thread_id,
                "delivery": "inline",
                "target": {"type": "uncommittedChanges"},
            },
        })),
        _ => None,
    }
}

fn codex_command_response(name: &str, error: Option<&str>) -> SlashCommandOutcome {
    if let Some(error) = error {
        return SlashCommandOutcome::Rejected {
            message: format!("/{name} failed: {error}"),
        };
    }

    if name == "compact" {
        SlashCommandOutcome::Completed {
            message: Some("Conversation context compacted.".to_string()),
        }
    } else {
        // review/start acknowledges creation before its inline review turn
        // reports the ordinary turn lifecycle.
        SlashCommandOutcome::Accepted
    }
}

fn delta_event(params: &Value, make: fn(String, String) -> Event) -> Vec<Event> {
    match (params["itemId"].as_str(), params["delta"].as_str()) {
        (Some(item_id), Some(delta)) => vec![make(item_id.to_string(), delta.to_string())],
        _ => Vec::new(),
    }
}

fn parse_thread_settings(result: &Value) -> ThreadSettings {
    ThreadSettings {
        model: result["model"].as_str().map(str::to_owned),
        approval: result["approvalPolicy"].as_str().map(str::to_owned),
        sandbox: result["sandbox"]["type"].as_str().map(str::to_owned),
        effort: result["reasoningEffort"].as_str().map(str::to_owned),
        tier: result["serviceTier"].as_str().map(str::to_owned),
    }
}

fn parse_models(result: &Value) -> Vec<ModelInfo> {
    result["data"]
        .as_array()
        .map(|data| {
            data.iter()
                .filter(|m| !m["hidden"].as_bool().unwrap_or(false))
                .filter_map(|m| {
                    let model = m["model"].as_str()?.to_string();
                    let display = m["displayName"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&model)
                        .to_string();
                    let tiers = m["serviceTiers"]
                        .as_array()
                        .map(|tiers| {
                            tiers
                                .iter()
                                .filter_map(|tier| {
                                    let id = tier["id"].as_str()?.to_string();
                                    let name = tier["name"]
                                        .as_str()
                                        .filter(|s| !s.is_empty())
                                        .unwrap_or(&id)
                                        .to_string();
                                    Some((id, name))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let default_tier = m["defaultServiceTier"].as_str().map(str::to_owned);

                    Some(ModelInfo {
                        model,
                        display,
                        tiers,
                        default_tier,
                        efforts: Vec::new(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One `thread/list` page as backend-neutral summaries, skipping
/// `own_thread` (the listing includes the thread this session just started).
fn parse_thread_summaries(result: &Value, own_thread: Option<&str>) -> Vec<SessionSummary> {
    result["data"]
        .as_array()
        .map(|data| {
            data.iter()
                .filter_map(|thread| {
                    let id = thread["id"].as_str()?.to_string();

                    if Some(id.as_str()) == own_thread {
                        return None;
                    }

                    let title = thread["name"]
                        .as_str()
                        .or_else(|| thread["preview"].as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| id.chars().take(8).collect());
                    // Wire timestamps are unix seconds; `recencyAt` advances
                    // when a turn starts, which matches "last active" better
                    // than `updatedAt` (background mutations move that).
                    let seconds = thread["recencyAt"]
                        .as_u64()
                        .or_else(|| thread["updatedAt"].as_u64())
                        .unwrap_or_default();
                    let branch = thread["gitInfo"]["branch"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned);

                    Some(SessionSummary {
                        id,
                        title,
                        branch,
                        last_active: UNIX_EPOCH + Duration::from_secs(seconds),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Flatten a resumed thread's `turns[].items` into replay entries:
/// conversation text stays, everything else (commands, file changes, tool
/// calls) collapses into counts.
fn parse_replay(turns: &Value) -> Vec<ReplayItem> {
    let mut items: Vec<ReplayItem> = Vec::new();
    let mut tools = 0usize;
    let flush = |items: &mut Vec<ReplayItem>, tools: &mut usize| {
        if *tools > 0 {
            items.push(ReplayItem::Tools { count: *tools });
            *tools = 0;
        }
    };

    for item in turns
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|turn| turn["items"].as_array())
        .flatten()
    {
        match item["type"].as_str() {
            Some("userMessage") => {
                let text = user_input_text(&item["content"]);

                if !text.is_empty() {
                    flush(&mut items, &mut tools);
                    items.push(ReplayItem::User { text });
                }
            }
            Some("agentMessage") => {
                let text = item["text"].as_str().unwrap_or_default().trim();

                if !text.is_empty() {
                    flush(&mut items, &mut tools);
                    items.push(ReplayItem::Agent {
                        text: text.to_string(),
                    });
                }
            }
            // Reasoning and hook records are working noise, not conversation.
            Some("reasoning") | Some("hookPrompt") | None => {}
            Some(_) => tools += 1,
        }
    }

    flush(&mut items, &mut tools);

    items
}

/// A user message item's `content` is an array of typed `UserInput` blocks.
fn user_input_text(content: &Value) -> String {
    let parts: Vec<&str> = content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"].as_str() == Some("text"))
        .filter_map(|block| block["text"].as_str())
        .collect();

    parts.join("\n").trim().to_string()
}

fn parse_item(item: &Value) -> Option<Item> {
    let id = item["id"].as_str().unwrap_or_default().to_string();
    let status = item["status"].as_str().map(str::to_owned);

    let parsed = match item["type"].as_str()? {
        "userMessage" => Item::UserMessage,
        "agentMessage" => Item::AgentMessage {
            id,
            text: item["text"].as_str().map(str::to_owned),
        },
        "reasoning" => Item::Reasoning {
            id,
            summary: item["summary"].as_array().map(|summary| {
                summary
                    .iter()
                    .filter_map(|part| part.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            }),
        },
        "commandExecution" => Item::CommandExecution {
            id,
            command: stringify_command(&item["command"]),
            aggregated_output: item["aggregatedOutput"].as_str().map(str::to_owned),
            status,
            exit_code: item["exitCode"].as_i64(),
        },
        "fileChange" => Item::FileChange {
            id,
            paths: file_change_paths(&item["changes"]),
            status,
        },
        kind => Item::Other {
            id,
            kind: kind.to_string(),
            title: tool_title(item),
            status,
        },
    };

    Some(parsed)
}

/// The protocol sends commands as either a shell string or an argv array.
fn stringify_command(command: &Value) -> String {
    match command {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|p| p.as_str().unwrap_or_default())
            .collect::<Vec<_>>()
            .join(" "),
        other => other.to_string(),
    }
}

fn file_change_paths(changes: &Value) -> String {
    let paths: Vec<&str> = changes
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|change| change["path"].as_str())
                .collect()
        })
        .unwrap_or_default();

    if paths.is_empty() {
        "(unknown files)".to_string()
    } else {
        paths.join(", ")
    }
}

/// Best-effort one-line label for an arbitrary tool item: MCP calls have
/// `server` + `tool`, dynamic tools `tool`, web searches `query`.
fn tool_title(item: &Value) -> String {
    match item["type"].as_str() {
        Some("contextCompaction") => return "Compacting conversation context".to_string(),
        Some("enteredReviewMode") => return "Entered review mode".to_string(),
        Some("exitedReviewMode") => return "Exited review mode".to_string(),
        _ => {}
    }

    let tool = item["tool"].as_str();

    match (item["server"].as_str(), tool) {
        (Some(server), Some(tool)) => format!("{server}/{tool}"),
        (None, Some(tool)) => tool.to_string(),
        _ => item["query"].as_str().unwrap_or_default().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_render_as_string_or_joined_argv() {
        assert_eq!(stringify_command(&json!("pytest -q")), "pytest -q");
        assert_eq!(
            stringify_command(&json!(["cargo", "check", "-p", "app"])),
            "cargo check -p app"
        );
    }

    #[test]
    fn model_catalog_keeps_visible_models_and_their_tiers() {
        let result = json!({
            "data": [
                {
                    "model": "gpt-a",
                    "displayName": "GPT A",
                    "hidden": false,
                    "serviceTiers": [{"id": "priority", "name": "Fast"}],
                    "defaultServiceTier": null
                },
                {"model": "gpt-b", "displayName": "GPT B", "hidden": true}
            ]
        });

        let models = parse_models(&result);

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model, "gpt-a");
        assert_eq!(models[0].tiers, vec![("priority".into(), "Fast".into())]);
    }

    #[test]
    fn thread_summaries_skip_own_thread_and_fall_back_to_id_titles() {
        let result = json!({
            "data": [
                {"id": "thr_live", "preview": "current"},
                {"id": "thr_a", "name": "Fix tests", "recencyAt": 1730831111,
                 "gitInfo": {"branch": "dev"}},
                {"id": "thr_b", "preview": "", "updatedAt": 1730750000}
            ],
            "nextCursor": null
        });

        let summaries = parse_thread_summaries(&result, Some("thr_live"));

        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].id, "thr_a");
        assert_eq!(summaries[0].title, "Fix tests");
        assert_eq!(summaries[0].branch.as_deref(), Some("dev"));
        assert_eq!(
            summaries[0].last_active,
            UNIX_EPOCH + Duration::from_secs(1730831111)
        );
        // Empty preview falls back to an id-prefix title.
        assert_eq!(summaries[1].title, "thr_b");
    }

    #[test]
    fn resumed_turns_replay_dialogue_and_collapse_tools() {
        let turns = json!([
            {"id": "turn1", "items": [
                {"id": "i1", "type": "userMessage",
                 "content": [{"type": "text", "text": "question"}]},
                {"id": "i2", "type": "commandExecution", "command": "ls"},
                {"id": "i3", "type": "reasoning", "summary": []},
                {"id": "i4", "type": "mcpToolCall", "server": "s", "tool": "t"},
                {"id": "i5", "type": "agentMessage", "text": "answer"}
            ]},
            {"id": "turn2", "items": [
                {"id": "i6", "type": "agentMessage", "text": "follow-up"}
            ]}
        ]);

        assert_eq!(
            parse_replay(&turns),
            vec![
                ReplayItem::User {
                    text: "question".into()
                },
                ReplayItem::Tools { count: 2 },
                ReplayItem::Agent {
                    text: "answer".into()
                },
                ReplayItem::Agent {
                    text: "follow-up".into()
                },
            ]
        );
    }

    #[test]
    fn unknown_items_become_titled_tool_cards() {
        let item = json!({
            "id": "call1",
            "type": "mcpToolCall",
            "server": "github",
            "tool": "search_issues",
            "status": "inProgress"
        });

        assert_eq!(
            parse_item(&item),
            Some(Item::Other {
                id: "call1".into(),
                kind: "mcpToolCall".into(),
                title: "github/search_issues".into(),
                status: Some("inProgress".into()),
            })
        );
    }

    #[test]
    fn command_requests_use_dedicated_compact_and_inline_review_methods() {
        assert_eq!(
            codex_command_request(100, "thr_1", "compact"),
            Some(json!({
                "jsonrpc": "2.0",
                "id": 100,
                "method": "thread/compact/start",
                "params": {"threadId": "thr_1"},
            }))
        );
        assert_eq!(
            codex_command_request(101, "thr_1", "review"),
            Some(json!({
                "jsonrpc": "2.0",
                "id": 101,
                "method": "review/start",
                "params": {
                    "threadId": "thr_1",
                    "delivery": "inline",
                    "target": {"type": "uncommittedChanges"},
                },
            }))
        );
        assert_eq!(codex_command_request(102, "thr_1", "unknown"), None);
        assert_eq!(
            codex_command_response("compact", None),
            SlashCommandOutcome::Completed {
                message: Some("Conversation context compacted.".into())
            }
        );
        assert_eq!(
            codex_command_response("review", None),
            SlashCommandOutcome::Accepted
        );
        assert_eq!(
            codex_command_response("review", Some("unsupported target")),
            SlashCommandOutcome::Rejected {
                message: "/review failed: unsupported target".into()
            }
        );
    }

    #[test]
    fn compaction_and_review_lifecycle_items_remain_visible() {
        for (kind, title) in [
            ("contextCompaction", "Compacting conversation context"),
            ("enteredReviewMode", "Entered review mode"),
            ("exitedReviewMode", "Exited review mode"),
        ] {
            assert_eq!(
                parse_item(&json!({"id": "item", "type": kind, "status": "completed"})),
                Some(Item::Other {
                    id: "item".into(),
                    kind: kind.into(),
                    title: title.into(),
                    status: Some("completed".into()),
                })
            );
        }
    }
}
