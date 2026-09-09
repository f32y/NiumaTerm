//! Authenticated calls to the Harness Remote API.

use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{COOKIE, HeaderValue, SET_COOKIE};
use reqwest::redirect::Policy;
use reqwest::{StatusCode, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use tungstenite::client::IntoClientRequest as _;
use tungstenite::handshake::client::Request;
use uuid::Uuid;

#[cfg(test)]
mod tests;

const CALL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CallError {
    Transport(String),
    Business { code: String, message: String },
}

impl CallError {
    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Transport(message) | Self::Business { message, .. } => message,
        }
    }
}

#[derive(Deserialize)]
struct ResponseMessage {
    result: CallResult,
}

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

/// One host's connection pool and authentication, shared by its sessions.
#[derive(Clone)]
pub(crate) struct ApiClient {
    http: Client,
    base: String,
    cookie: Option<HeaderValue>,
}

impl ApiClient {
    /// Exchange the startup URL's process token for a host-scoped cookie.
    /// The printed URL points at the browser entry page; API paths belong on
    /// its origin, outside the token query. Cookies stay in memory and redirects
    /// are disabled so credentials cannot travel to another server.
    pub(crate) fn new(address: String) -> Result<Self, String> {
        let url = Url::parse(&address)
            .map_err(|_| "the harness printed an invalid startup URL".to_string())?;
        let http = Client::builder()
            .timeout(CALL_TIMEOUT)
            .no_proxy()
            .redirect(Policy::none())
            .build()
            .map_err(|error| format!("could not create the DeepSeek client: {error}"))?;
        let cookie = if url.query_pairs().any(|(name, _)| name == "token") {
            let response = http
                .get(url.clone())
                .send()
                .map_err(|error| format!("the harness login failed: {}", error.without_url()))?;
            if response.status() != StatusCode::SEE_OTHER {
                return Err(format!(
                    "the harness login was refused with status {}",
                    response.status()
                ));
            }
            let cookies: Vec<&str> = response
                .headers()
                .get_all(SET_COOKIE)
                .iter()
                .filter_map(|value| value.to_str().ok()?.split(';').next())
                .filter(|value| value.contains('='))
                .collect();
            if cookies.is_empty() {
                return Err("the harness login returned no session cookie".to_string());
            }
            let mut cookie = HeaderValue::from_str(&cookies.join("; "))
                .map_err(|_| "the harness login returned an invalid cookie".to_string())?;
            cookie.set_sensitive(true);
            Some(cookie)
        } else {
            None
        };

        Ok(Self {
            http,
            base: url.origin().ascii_serialization(),
            cookie,
        })
    }

    /// Invoke a Remote method with its generated named arguments.
    pub(crate) fn call(&self, method: &str, args: Value) -> Result<Value, CallError> {
        self.call_with_timeout(method, args, CALL_TIMEOUT)
    }

    /// Session command methods take one named request object.
    pub(crate) fn request(&self, method: &str, request: Value) -> Result<Value, CallError> {
        self.call(method, json!({ "request": request }))
    }

    /// Close cleanup uses a shorter deadline so a stalled host cannot retain
    /// its detached worker for the entire foreground timeout.
    pub(crate) fn call_with_timeout(
        &self,
        method: &str,
        args: Value,
        timeout: Duration,
    ) -> Result<Value, CallError> {
        self.send(method, json!({ "args": args }), timeout)
    }

    fn send(&self, method: &str, payload: Value, timeout: Duration) -> Result<Value, CallError> {
        let request = json!({
            "type": "client-request",
            "rpcId": Uuid::new_v4().to_string(),
            "method": method,
            "payload": payload,
        });
        let mut pending = self
            .http
            .post(format!("{}/api/{method}", self.base))
            .header("content-type", "application/json")
            .body(request.to_string())
            .timeout(timeout);
        if let Some(cookie) = &self.cookie {
            pending = pending.header(COOKIE, cookie.clone());
        }
        let response = pending.send().map_err(|error| {
            CallError::Transport(format!("{method} could not be sent: {error}"))
        })?;
        if !response.status().is_success() {
            return Err(CallError::Transport(format!(
                "{method} was refused by the harness host with status {}",
                response.status()
            )));
        }
        let body = response.text().map_err(|error| {
            CallError::Transport(format!("{method} returned an unreadable response: {error}"))
        })?;
        let answer = serde_json::from_str::<ResponseMessage>(&body).map_err(|error| {
            CallError::Transport(format!("{method} returned an unreadable response: {error}"))
        })?;
        match (answer.result.ok, answer.result.value, answer.result.error) {
            (true, value, _) => Ok(value.unwrap_or(Value::Null)),
            (false, _, Some(error)) => Err(CallError::Business {
                code: error.code,
                message: error.message,
            }),
            _ => Err(CallError::Transport(format!(
                "{method} returned a result that was neither a value nor an error"
            ))),
        }
    }

    /// Event replies carry their stream generation and event identity, so an
    /// answer from an earlier connection cannot settle a newer request.
    pub(crate) fn respond_event(
        &self,
        client_id: &str,
        event_id: &str,
        outcome: Value,
    ) -> Result<(), CallError> {
        self.call(
            "$events/result",
            json!({ "clientId": client_id, "eventId": event_id, "outcome": outcome }),
        )
        .map(|_| ())
    }

    pub(crate) fn stream_request(&self) -> Result<Request, String> {
        let mut url = Url::parse(&self.base).map_err(|error| error.to_string())?;
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(scheme)
            .map_err(|_| "the harness URL has no WebSocket scheme".to_string())?;
        url.set_path("/api/remote.mux");
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|error| error.to_string())?;
        if let Some(cookie) = &self.cookie {
            request.headers_mut().insert(COOKIE, cookie.clone());
        }
        Ok(request)
    }
}
