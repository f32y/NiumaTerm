#[cfg(test)]
#[path = "title_generation_tests.rs"]
mod title_generation_tests;

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::{Instant, timeout_at};

use crate::codex::app_server::ThreadProfile;
use crate::codex::app_server::host::{CodexHost, HOST_EXIT_METHOD, RegistrationId};
use crate::codex::app_server::protocol::thread_start_params;
use crate::session::naming::provisional_title;
use crate::workspace::AgentWorkspace;

const GENERATED_TITLE_CHARS: usize = 36;
const TITLE_PROMPT_CHARS: usize = 2_000;
const DEFAULT_TITLE_MODEL: &str = "gpt-5.6-luna";
const TITLE_THREAD_START_RPC_ID: u64 = 1;
const TITLE_TURN_START_RPC_ID: u64 = 2;
const TITLE_TURN_INTERRUPT_RPC_ID: u64 = 3;
const TITLE_THREAD_UNSUBSCRIBE_RPC_ID: u64 = 4;
const TITLE_GENERATION_CANCEL_METHOD: &str = "nmt/codexTitleGenerationCancel";

pub(super) const TITLE_GENERATION_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const TITLE_GENERATION_RESULT_METHOD: &str = "nmt/codexTitleGenerationResult";

type Delivery = Arc<dyn Fn(Value) + Send + Sync>;

pub(super) struct TitleGenerationRequest {
    pub(super) generation_id: u64,
    pub(super) root_thread_id: String,
    pub(super) provisional_title: String,
    pub(super) prompt: String,
    pub(super) profile: ThreadProfile,
    pub(super) workspace: AgentWorkspace,
}

pub(super) struct TitleGenerationHandle {
    pub(super) generation_id: u64,
    pub(super) root_thread_id: String,
    pub(super) provisional_title: String,
    cancel_tx: UnboundedSender<Value>,
}

impl TitleGenerationHandle {
    pub(super) fn cancel(&self) {
        let _ = self
            .cancel_tx
            .send(json!({"method": TITLE_GENERATION_CANCEL_METHOD}));
    }

    pub(super) fn accepts(
        &self,
        result: &TitleGenerationResult,
        active_thread_id: Option<&str>,
    ) -> bool {
        self.generation_id == result.generation_id
            && self.root_thread_id == result.root_thread_id
            && self.provisional_title == result.provisional_title
            && active_thread_id == Some(result.root_thread_id.as_str())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct TitleGenerationResult {
    pub(super) generation_id: u64,
    pub(super) root_thread_id: String,
    pub(super) provisional_title: String,
    pub(super) generated_title: Option<String>,
}

impl TitleGenerationResult {
    pub(super) fn resolved_title(&self) -> &str {
        self.generated_title
            .as_deref()
            .unwrap_or(&self.provisional_title)
    }
}

/// Cancellation arrives as a message rather than by aborting the task, so the
/// cleanup that interrupts the title turn and detaches the registration runs.
pub(super) fn start_title_generation(
    host: Arc<CodexHost>,
    deliver: Delivery,
    request: TitleGenerationRequest,
) -> TitleGenerationHandle {
    let (tx, rx) = unbounded_channel();
    let callback_tx = tx.clone();

    let registration_id = host.register(move |message| {
        let _ = callback_tx.send(message);
    });

    let generation_id = request.generation_id;
    let root_thread_id = request.root_thread_id.clone();
    let provisional_title = request.provisional_title.clone();

    nmt_platform::runtime().spawn(run_title_generation(
        host,
        registration_id,
        rx,
        deliver,
        request,
    ));

    TitleGenerationHandle {
        generation_id,
        root_thread_id,
        provisional_title,
        cancel_tx: tx,
    }
}

pub(super) fn parse_title_generation_result(
    method: &str,
    params: &Value,
) -> Option<TitleGenerationResult> {
    if method != TITLE_GENERATION_RESULT_METHOD {
        return None;
    }

    Some(TitleGenerationResult {
        generation_id: params["generationId"].as_u64()?,
        root_thread_id: params["rootThreadId"].as_str()?.to_string(),
        provisional_title: params["provisionalTitle"].as_str()?.to_string(),
        generated_title: params["generatedTitle"].as_str().map(str::to_owned),
    })
}

async fn run_title_generation(
    host: Arc<CodexHost>,
    registration_id: RegistrationId,
    mut rx: UnboundedReceiver<Value>,
    deliver: Delivery,
    request: TitleGenerationRequest,
) {
    let TitleGenerationRequest {
        generation_id,
        root_thread_id,
        provisional_title,
        prompt,
        profile,
        workspace,
    } = request;

    let deadline = Instant::now() + TITLE_GENERATION_TIMEOUT;

    let mut title_thread_id = None;
    let mut title_turn_id = None;
    let mut output = String::new();
    let mut cancelled = false;
    let mut completed = false;

    if host
        .send(
            registration_id,
            title_thread_start_request(TITLE_THREAD_START_RPC_ID, &profile, &workspace),
        )
        .is_ok()
    {
        while let Ok(Some(message)) = timeout_at(deadline, rx.recv()).await {
            if message["method"].as_str() == Some(TITLE_GENERATION_CANCEL_METHOD) {
                cancelled = true;

                break;
            }

            if message["method"].as_str() == Some(HOST_EXIT_METHOD) {
                break;
            }

            if let Some(id) = message["id"].as_u64() {
                if message["method"].is_string() {
                    answer_unsupported_server_request(&host, registration_id, id);

                    continue;
                }

                if message["error"].is_object() {
                    break;
                }

                if id == TITLE_THREAD_START_RPC_ID {
                    let Some(thread_id) = message["result"]["thread"]["id"].as_str() else {
                        break;
                    };

                    title_thread_id = Some(thread_id.to_string());

                    if host
                        .send(
                            registration_id,
                            title_turn_start_request(TITLE_TURN_START_RPC_ID, thread_id, &prompt),
                        )
                        .is_err()
                    {
                        break;
                    }
                } else if id == TITLE_TURN_START_RPC_ID {
                    title_turn_id = message["result"]["turn"]["id"].as_str().map(str::to_owned);
                }

                continue;
            }

            let method = message["method"].as_str().unwrap_or_default();
            let params = &message["params"];

            if params["threadId"].as_str() != title_thread_id.as_deref() {
                continue;
            }

            match method {
                "turn/started" => {
                    title_turn_id = params["turn"]["id"].as_str().map(str::to_owned);
                }
                "item/agentMessage/delta" => {
                    if let Some(delta) = params["delta"].as_str() {
                        output.push_str(delta);
                    }
                }
                "item/completed" if params["item"]["type"].as_str() == Some("agentMessage") => {
                    if let Some(text) = params["item"]["text"].as_str() {
                        output.clear();

                        output.push_str(text);
                    }
                }
                "turn/completed" => {
                    completed = params["turn"]["status"].as_str() == Some("completed");

                    break;
                }
                "error" => break,
                _ => {}
            }
        }
    }

    let generated_title = completed
        .then(|| generated_title_from_message(&output))
        .flatten();

    finish_title_thread(
        &host,
        registration_id,
        title_thread_id.as_deref(),
        title_turn_id.as_deref(),
        !completed,
    );

    if !cancelled {
        deliver(json!({
            "method": TITLE_GENERATION_RESULT_METHOD,
            "params": {
                "generationId": generation_id,
                "rootThreadId": root_thread_id,
                "provisionalTitle": provisional_title,
                "generatedTitle": generated_title,
            },
        }));
    }
}

fn answer_unsupported_server_request(
    host: &CodexHost,
    registration_id: RegistrationId,
    request_id: u64,
) {
    let _ = host.send(
        registration_id,
        json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": -32601, "message": "not supported during title generation"},
        }),
    );
}

fn finish_title_thread(
    host: &CodexHost,
    registration_id: RegistrationId,
    thread_id: Option<&str>,
    turn_id: Option<&str>,
    interrupt: bool,
) {
    if interrupt && let (Some(thread_id), Some(turn_id)) = (thread_id, turn_id) {
        let _ = host.send(
            registration_id,
            json!({
                "jsonrpc": "2.0",
                "id": TITLE_TURN_INTERRUPT_RPC_ID,
                "method": "turn/interrupt",
                "params": {"threadId": thread_id, "turnId": turn_id},
            }),
        );
    }

    if let Some(thread_id) = thread_id {
        let _ = host.send(
            registration_id,
            json!({
                "jsonrpc": "2.0",
                "id": TITLE_THREAD_UNSUBSCRIBE_RPC_ID,
                "method": "thread/unsubscribe",
                "params": {"threadId": thread_id},
            }),
        );
    }

    host.detach(registration_id);
}

/// Derive the immediate local title from the first ordinary prompt. Commands
/// leave the conversation unnamed because they describe an operation rather
/// than the subject the user wants to discuss.
pub(crate) fn provisional_title_from_prompt(prompt: &str) -> Option<String> {
    provisional_title(prompt, None)
}

pub(super) fn title_thread_start_request(
    rpc_id: u64,
    profile: &ThreadProfile,
    workspace: &AgentWorkspace,
) -> Value {
    let mut params = thread_start_params(profile, workspace);

    params["ephemeral"] = json!(true);
    params["approvalPolicy"] = json!("never");
    params["allowProviderModelFallback"] = json!(true);

    if profile.provider.is_none() {
        params["model"] = json!(DEFAULT_TITLE_MODEL);
    }

    let config = params
        .as_object_mut()
        .expect("thread parameters are an object")
        .entry("config")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .expect("thread config is an object");

    for key in [
        "features.enable_fanout",
        "features.hooks",
        "features.multi_agent",
        "features.multi_agent_v2",
        "features.plugins",
        "features.shell_snapshot",
        "features.tool_suggest",
    ] {
        config.insert(key.to_string(), json!(false));
    }

    config.insert("web_search".to_string(), json!("disabled"));

    json!({
        "jsonrpc": "2.0",
        "id": rpc_id,
        "method": "thread/start",
        "params": params,
    })
}

pub(super) fn title_turn_start_request(rpc_id: u64, thread_id: &str, prompt: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": rpc_id,
        "method": "turn/start",
        "params": {
            "threadId": thread_id,
            "turnTrigger": "thread_title",
            "input": [{"type": "text", "text": title_prompt(prompt)}],
            "approvalPolicy": "never",
            "sandboxPolicy": {"type": "readOnly"},
            "effort": "low",
            "serviceTier": null,
            "outputSchema": {
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": GENERATED_TITLE_CHARS,
                    },
                },
                "required": ["title"],
                "additionalProperties": false,
            },
        },
    })
}

pub(super) fn generated_title_from_message(message: &str) -> Option<String> {
    let value: Value = serde_json::from_str(message.trim()).ok()?;

    let normalized = value["title"]
        .as_str()?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    if normalized.is_empty() || normalized.chars().count() > GENERATED_TITLE_CHARS {
        return None;
    }

    Some(normalized)
}

fn title_prompt(prompt: &str) -> String {
    let prompt: String = prompt.trim().chars().take(TITLE_PROMPT_CHARS).collect();

    [
        "Generate a concise UI title for the coding task in the user prompt.",
        "Use at most 36 characters and fewer than five words when practical.",
        "Use the user's language and preserve any issue identifier.",
        "Prefer an action verb for a requested change and a discovery verb for a question.",
        "Do not answer the prompt or perform the task.",
        "Return plain text in the structured title field without quotes, markup, or trailing punctuation.",
        "",
        "User prompt:",
        &prompt,
    ]
    .join("\n")
}
