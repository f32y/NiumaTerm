//! Unary half of the DeepSeek Harness local interface: `POST /api/<method>`
//! carrying a request message, answering a response message.
//!
//! Business failures arrive as a successful HTTP response holding an error
//! branch, so a non-200 status means the carrier itself failed and nothing
//! about the request was understood. The two are kept apart here because a
//! carrier failure usually means the host died, while a business error is
//! something the tab reports and continues from.

use std::time::Duration;

use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{Value, json};

/// The host rejects any media type other than JSON to force a CORS preflight it
/// never answers, which is what stops a web page from driving it blind. A client
/// that omits this header gets 415 rather than a business error.
const JSON_MEDIA_TYPE: &str = "application/json";

/// Long enough for the host to start a session or admit a prompt, short enough
/// that a wedged host surfaces as a failed tab rather than a frozen one.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a call did not produce a value. The message is already user-facing:
/// the harness writes its own business errors, and the carrier ones name what
/// could not be reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CallError {
    /// The host could not be reached, answered a non-JSON body, or answered a
    /// status that describes the carrier rather than the request.
    Transport(String),
    /// The host understood the request and refused it. `code` is its own closed
    /// vocabulary, kept so a caller can branch without matching on prose.
    Business { code: String, message: String },
}

impl CallError {
    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Transport(message) | Self::Business { message, .. } => message,
        }
    }
}

/// Response message. `rpcId` echoes the request's, which matters on the
/// downlinks where several answers are in flight; a unary POST answers on its
/// own connection, so the echo is not re-checked here.
#[derive(Deserialize)]
struct ResponseMessage {
    result: CallResult,
}

/// `ok` is the discriminant, and the branch it selects decides which of the
/// other two fields is present. Both are optional here so a malformed pairing
/// reports as a readable failure rather than a parse error naming no method.
#[derive(Deserialize)]
struct CallResult {
    ok: bool,
    value: Option<Value>,
    error: Option<BusinessError>,
}

#[derive(Deserialize)]
struct BusinessError {
    code: String,
    message: String,
}

/// Client for one running host. Cloning shares the connection pool, so every
/// tab's calls reuse the same loopback connections.
#[derive(Clone)]
pub(crate) struct ApiClient {
    http: Client,
    base: String,
}

impl ApiClient {
    /// `base` is the origin the host printed, without a trailing slash.
    pub(crate) fn new(base: String) -> Result<Self, String> {
        Client::builder()
            .timeout(CALL_TIMEOUT)
            // The host is on loopback; an inherited proxy would send the
            // request somewhere else entirely.
            .no_proxy()
            .build()
            .map(|http| Self { http, base })
            .map_err(|error| format!("could not create the DeepSeek client: {error}"))
    }

    pub(crate) fn call(&self, method: &str, payload: Value) -> Result<Value, CallError> {
        let rpc_id = uuid::Uuid::new_v4().to_string();
        let request = json!({
            "type": "client-request",
            "rpcId": rpc_id,
            "method": method,
            "payload": payload,
        });
        let response = self
            .http
            .post(format!("{}/api/{method}", self.base))
            .header("content-type", JSON_MEDIA_TYPE)
            .body(request.to_string())
            .send()
            .map_err(|error| {
                CallError::Transport(format!("{method} could not be sent: {error}"))
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(CallError::Transport(format!(
                "{method} was refused by the harness host with status {status}"
            )));
        }

        let body = response.text().map_err(|error| {
            CallError::Transport(format!("{method} returned an unreadable response: {error}"))
        })?;
        let answer = serde_json::from_str::<ResponseMessage>(&body).map_err(|error| {
            CallError::Transport(format!("{method} returned an unreadable response: {error}"))
        })?;

        match (answer.result.ok, answer.result.value, answer.result.error) {
            (true, Some(value), _) => Ok(value),
            (false, _, Some(error)) => Err(CallError::Business {
                code: error.code,
                message: error.message,
            }),
            _ => Err(CallError::Transport(format!(
                "{method} returned a result that was neither a value nor an error"
            ))),
        }
    }

    /// Answer a frame the harness is blocked on. The reply is correlated by
    /// echoing the frame's own `rpcId`, never a freshly minted one.
    ///
    /// The receipt is the only report there is. An answer the host rejects
    /// produces no error frame and no second ask — whatever raised the question
    /// simply keeps waiting — so this returns the refusal instead of assuming
    /// the answer landed.
    pub(crate) fn respond(&self, rpc_id: &str, value: Value) -> Result<(), CallError> {
        let answer = json!({
            "type": "client-response",
            "rpcId": rpc_id,
            "result": { "ok": true, "value": value },
        });
        let body = self
            .http
            .post(format!("{}/api/respond", self.base))
            .header("content-type", JSON_MEDIA_TYPE)
            .body(answer.to_string())
            .send()
            .map_err(|error| {
                CallError::Transport(format!("the answer could not be sent: {error}"))
            })?
            .text()
            .map_err(|error| {
                CallError::Transport(format!("the answer receipt was unreadable: {error}"))
            })?;
        let receipt = serde_json::from_str::<Value>(&body).map_err(|error| {
            CallError::Transport(format!("the answer receipt was unreadable: {error}"))
        })?;

        if receipt["accepted"] == Value::Bool(true) {
            return Ok(());
        }

        // `bad-response` means the shape was wrong and `not-pending` that the
        // question was already settled; both leave nothing further to react to.
        let reason = receipt["reason"].as_str().unwrap_or("unknown");
        Err(CallError::Transport(format!(
            "the harness did not accept the answer: {reason}"
        )))
    }
}
