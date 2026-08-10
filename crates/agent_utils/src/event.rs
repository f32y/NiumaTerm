use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::AgentRoute;
use crate::claude_code::hook::normalize as normalize_claude;
use crate::codex::hook::normalize as normalize_codex;
use crate::process::AGENT_HOOK_PROTOCOL_VERSION;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgentRuntimeStatus {
    Running,
    NeedsInput,
    #[default]
    Idle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentEventKind {
    SessionStarted,
    PromptSubmitted,
    ToolStarted,
    PermissionRequested,
    ToolFinished,
    Stopped,
}

/// Raw stdin hook payload forwarded by the hook CLI, normalized per agent.
/// Both supported CLIs emit the same payload schema; the adapters in
/// `codex`/`claude_code` differ only in turn identity and presentation
/// strings, and always fail open.
#[derive(Serialize, Deserialize)]
pub struct RawAgentHookMessage {
    pub action: String,
    pub version: u32,
    pub token: String,
    pub route: String,
    pub payload: Value,
}

impl RawAgentHookMessage {
    pub fn into_event(self, expected_token: &str) -> Option<AgentEvent> {
        let normalize = match self.action.as_str() {
            "codex_hook" => normalize_codex,
            "claude_hook" => normalize_claude,
            _ => return None,
        };

        normalize(
            self.payload,
            &self.route,
            &self.token,
            self.version,
            expected_token,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentOwner {
    pub agent: String,
    pub session_id: String,
    pub turn_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentEvent {
    pub route: AgentRoute,
    pub agent: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub kind: AgentEventKind,
    pub title: String,
    pub body: String,
}

/// Untrusted fields supplied by a Hook transport. Successful validation is the
/// only way to construct a reducer event from process-boundary input.
pub struct AgentEventInput<'a> {
    pub route: &'a str,
    pub token: &'a str,
    pub version: u32,
    pub agent: &'a str,
    pub session_id: &'a str,
    pub turn_id: Option<&'a str>,
    pub kind: AgentEventKind,
    pub title: &'a str,
    pub body: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentValidationError {
    InvalidRoute,
    InvalidToken,
    UnsupportedVersion,
    InvalidAgent,
    InvalidSessionId,
    InvalidTurnId,
}

impl AgentEvent {
    pub fn validate(
        input: AgentEventInput<'_>,
        expected_token: &str,
    ) -> Result<Self, AgentValidationError> {
        if input.version != AGENT_HOOK_PROTOCOL_VERSION {
            return Err(AgentValidationError::UnsupportedVersion);
        }

        if expected_token.is_empty() || !constant_time_eq(input.token, expected_token) {
            return Err(AgentValidationError::InvalidToken);
        }

        let route = AgentRoute::parse(input.route)?;

        validate_identity(
            input.agent,
            MAX_AGENT_BYTES,
            AgentValidationError::InvalidAgent,
        )?;

        validate_identity(
            input.session_id,
            MAX_PROVIDER_ID_BYTES,
            AgentValidationError::InvalidSessionId,
        )?;

        match (input.kind, input.turn_id) {
            (AgentEventKind::SessionStarted, None) => {}
            (AgentEventKind::SessionStarted, Some(_)) | (_, None) => {
                return Err(AgentValidationError::InvalidTurnId);
            }
            (_, Some(turn_id)) => validate_identity(
                turn_id,
                MAX_PROVIDER_ID_BYTES,
                AgentValidationError::InvalidTurnId,
            )?,
        }

        Ok(Self {
            route,
            agent: input.agent.to_owned(),
            session_id: input.session_id.to_owned(),
            turn_id: input.turn_id.map(str::to_owned),
            kind: input.kind,
            title: normalize_title(input.title),
            body: normalize_body(input.body),
        })
    }

    pub(super) fn owner(&self) -> Option<AgentOwner> {
        Some(AgentOwner {
            agent: self.agent.clone(),
            session_id: self.session_id.clone(),
            turn_id: self.turn_id.clone()?,
        })
    }
}

pub(super) fn validate_identity(
    value: &str,
    max_bytes: usize,
    error: AgentValidationError,
) -> Result<(), AgentValidationError> {
    if value.is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        Err(error)
    } else {
        Ok(())
    }
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();

    let mut different = left.len() ^ right.len();

    let length = left.len().max(right.len());

    for index in 0..length {
        different |= usize::from(*left.get(index).unwrap_or(&0) ^ *right.get(index).unwrap_or(&0));
    }

    different == 0
}

pub fn normalize_title(value: &str) -> String {
    normalize_presentation(value, MAX_TITLE_CHARS, false)
}

pub fn normalize_body(value: &str) -> String {
    normalize_presentation(value, MAX_BODY_CHARS, true)
}

fn normalize_presentation(value: &str, max_chars: usize, preserve_newlines: bool) -> String {
    let mut normalized = String::new();
    let mut last_was_space = false;
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        let ch = if preserve_newlines && ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            '\n'
        } else if preserve_newlines && ch == '\n' {
            '\n'
        } else if ch.is_control() {
            ' '
        } else {
            ch
        };

        if ch == ' ' {
            if !last_was_space {
                normalized.push(ch);
            }

            last_was_space = true;
        } else {
            normalized.push(ch);
            last_was_space = false;
        }
    }
    normalized.trim().chars().take(max_chars).collect()
}

pub(super) const MAX_ROUTE_BYTES: usize = 128;
const MAX_AGENT_BYTES: usize = 32;
const MAX_PROVIDER_ID_BYTES: usize = 256;
pub(super) const MAX_TITLE_CHARS: usize = 256;
const MAX_BODY_CHARS: usize = 4_096;
