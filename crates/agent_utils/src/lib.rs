pub mod claude_code {
    pub mod hook;
}
mod hook_store;
pub mod codex {
    pub mod hook;
    #[cfg(target_os = "windows")]
    pub mod usage_fetcher;
}

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::{env, io, process};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use getrandom::fill;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::claude_code::hook::normalize as normalize_claude;
use crate::codex::hook::normalize as normalize_codex;

pub const AGENT_HOOK_PROTOCOL_VERSION: u32 = 1;
pub const COMPLETION_QUIET_WINDOW: Duration = Duration::from_millis(1_500);
pub const ACTIVE_STATE_STALE_AFTER: Duration = Duration::from_secs(30 * 60);

const MAX_ROUTE_BYTES: usize = 128;
const MAX_AGENT_BYTES: usize = 32;
const MAX_PROVIDER_ID_BYTES: usize = 256;
const MAX_TITLE_CHARS: usize = 256;
const MAX_BODY_CHARS: usize = 4_096;

pub const AGENT_ROUTE_ENV: &str = "NMT_AGENT_ROUTE";
pub const AGENT_HOOK_TOKEN_ENV: &str = "NMT_AGENT_HOOK_TOKEN";
pub const AGENT_HOOK_VERSION_ENV: &str = "NMT_AGENT_HOOK_VERSION";
pub const AGENT_HOOK_EXE_ENV: &str = "NMT_AGENT_HOOK_EXE";
pub const AGENT_TESTING_ENV: &str = "NMT_TESTING";

pub struct AgentProcess {
    nonce: String,
    hook_token: String,
    hook_executable: OnceLock<String>,
    testing: AtomicBool,
    next_route: AtomicU64,
    next_notification: AtomicU64,
}

impl AgentProcess {
    fn new() -> Self {
        let mut nonce = [0u8; 16];
        let mut hook_token = [0u8; 32];

        fill(&mut nonce).expect("Windows cryptographic random source");
        fill(&mut hook_token).expect("Windows cryptographic random source");

        Self {
            nonce: format!("{:x}-{}", process::id(), hex(&nonce)),
            hook_token: hex(&hook_token),
            hook_executable: OnceLock::new(),
            testing: AtomicBool::new(false),
            next_route: AtomicU64::new(1),
            next_notification: AtomicU64::new(1),
        }
    }

    pub fn allocate_route(&self) -> AgentRoute {
        let counter = self.next_route.fetch_add(1, Ordering::Relaxed);
        AgentRoute(format!("{}-{counter:x}", self.nonce))
    }

    pub fn next_notification_counter(&self) -> u64 {
        self.next_notification.fetch_add(1, Ordering::Relaxed)
    }

    pub fn hook_token(&self) -> &str {
        &self.hook_token
    }

    pub fn process_instance(&self) -> &str {
        &self.nonce
    }

    pub fn set_testing(&self, testing: bool) {
        self.testing.store(testing, Ordering::Relaxed);
    }

    /// Absolute path of the hook CLI binary, exported to every pane so
    /// externally configured agent hooks can locate it via `$NMT_AGENT_HOOK_EXE`
    /// without baking an install path into their configuration.
    pub fn set_hook_executable(&self, path: String) {
        let _ = self.hook_executable.set(path);
    }

    /// Installers use the same absolute binary path exported to pane children,
    /// so their registrations keep working when NiumaTerm is installed in a
    /// directory that is not on `PATH`.
    pub fn hook_executable(&self) -> Option<&str> {
        self.hook_executable.get().map(String::as_str)
    }

    pub fn environment_for(&self, route: &AgentRoute) -> Vec<(String, String)> {
        let mut environment = vec![
            (AGENT_ROUTE_ENV.into(), route.as_str().into()),
            (AGENT_HOOK_TOKEN_ENV.into(), self.hook_token.clone()),
            (
                AGENT_HOOK_VERSION_ENV.into(),
                AGENT_HOOK_PROTOCOL_VERSION.to_string(),
            ),
        ];

        if let Some(path) = self.hook_executable.get() {
            environment.push((AGENT_HOOK_EXE_ENV.into(), path.clone()));
        }

        if self.testing.load(Ordering::Relaxed) {
            environment.push((AGENT_TESTING_ENV.into(), "1".into()));
        }

        environment
    }
}

pub fn agent_process() -> &'static AgentProcess {
    static PROCESS: OnceLock<AgentProcess> = OnceLock::new();

    PROCESS.get_or_init(AgentProcess::new)
}

pub fn request_native_delivery(
    exact_visible_route: Option<&AgentRoute>,
    notification_route: &AgentRoute,
) -> bool {
    exact_visible_route != Some(notification_route)
}

pub fn exact_window_is_active(
    gpui_active: bool,
    foreground_matches_window: bool,
    foreground_minimized: bool,
) -> bool {
    gpui_active && foreground_matches_window && !foreground_minimized
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut output = String::with_capacity(bytes.len() * 2);

    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }

    output
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AgentRoute(String);

impl AgentRoute {
    pub fn parse(value: &str) -> Result<Self, AgentValidationError> {
        validate_identity(value, MAX_ROUTE_BYTES, AgentValidationError::InvalidRoute)?;

        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgentRuntimeStatus {
    Running,
    NeedsInput,
    #[default]
    Idle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEventKind {
    SessionStarted,
    PromptSubmitted,
    ToolStarted,
    PermissionRequested,
    ToolFinished,
    Stopped,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentEventWire {
    pub route: String,
    pub agent: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub kind: AgentEventKind,
    pub title: String,
    pub body: String,
}

/// Raw stdin hook payload forwarded by the hook CLI, normalized per agent.
/// Both supported CLIs emit the same payload schema; the adapters in
/// `codex`/`claude_code` differ only in turn identity and presentation
/// strings, and always fail open.
#[derive(Serialize, Deserialize)]
pub struct RawAgentHookEnvelope {
    pub action: String,
    pub version: u32,
    pub token: String,
    pub route: String,
    pub payload: Value,
}

impl RawAgentHookEnvelope {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookInstallStatus {
    /// Every event is registered with the current hook command.
    Installed,
    /// NiumaTerm entries exist but differ from the current command (for
    /// example a legacy absolute-path install) or miss events; reinstalling
    /// migrates them.
    Stale,
    NotInstalled,
}

/// Build a Windows hook command that remains valid when an agent executes it
/// through either cmd.exe or PowerShell.
pub fn build_windows_hook_command(executable: &str, argument: &str) -> io::Result<String> {
    let system_root = env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());

    build_windows_hook_command_for(executable, argument, &system_root)
}

fn build_windows_hook_command_for(
    executable: &str,
    argument: &str,
    system_root: &str,
) -> io::Result<String> {
    if executable.is_empty() || executable.contains(['\0', '\r', '\n']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "hook executable path is invalid",
        ));
    }

    if argument.is_empty()
        || !argument
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "hook argument is not shell-safe",
        ));
    }

    // A bare path containing only cmd/PowerShell-safe characters starts
    // without another interpreter and avoids per-event PowerShell startup.
    if executable.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'\\' | b'~' | b'-')
    }) {
        return Ok(format!("{executable} {argument}"));
    }

    // Spaces and cmd metacharacters such as `%` and `^` cannot be made safe by
    // quoting alone. Encoding the PowerShell invocation keeps the executable
    // path out of the outer agent shell's parser.
    let quoted = executable.replace('\'', "''");
    let script = format!("& '{quoted}' {argument}; exit $LASTEXITCODE");

    let encoded = STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );

    let powershell = format!(
        "{}/System32/WindowsPowerShell/v1.0/powershell.exe",
        system_root.trim_end_matches(['\\', '/']).replace('\\', "/")
    );

    Ok(format!(
        "{powershell} -NoProfile -ExecutionPolicy Bypass -EncodedCommand {encoded}"
    ))
}

/// Match a marker in either a plain hook command or the decoded payload of a
/// PowerShell `-EncodedCommand` launcher.
pub fn hook_command_contains(command: &str, marker: &str) -> bool {
    command.contains(marker)
        || decode_powershell_command(command).is_some_and(|decoded| decoded.contains(marker))
}

fn decode_powershell_command(command: &str) -> Option<String> {
    let mut parts = command.split_whitespace();

    let encoded = loop {
        if parts.next()?.eq_ignore_ascii_case("-EncodedCommand") {
            break parts.next()?;
        }
    };

    let bytes = STANDARD.decode(encoded).ok()?;

    let mut chunks = bytes.chunks_exact(2);

    let units = chunks
        .by_ref()
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();

    if !chunks.remainder().is_empty() {
        return None;
    }

    String::from_utf16(&units).ok()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentEventEnvelope {
    pub action: String,
    pub version: u32,
    pub token: String,
    pub event: AgentEventWire,
}

impl AgentEventEnvelope {
    pub fn from_event(event: AgentEvent, token: String) -> Self {
        Self {
            action: "agent_event".into(),
            version: AGENT_HOOK_PROTOCOL_VERSION,
            token,
            event: AgentEventWire {
                route: event.route.0,
                agent: event.agent,
                session_id: event.session_id,
                turn_id: event.turn_id,
                kind: event.kind,
                title: event.title,
                body: event.body,
            },
        }
    }

    pub fn into_event(self, expected_token: &str) -> Result<AgentEvent, AgentValidationError> {
        if self.action != "agent_event" {
            return Err(AgentValidationError::UnsupportedAction);
        }

        AgentEvent::validate(
            AgentEventInput {
                route: &self.event.route,
                token: &self.token,
                version: self.version,
                agent: &self.event.agent,
                session_id: &self.event.session_id,
                turn_id: self.event.turn_id.as_deref(),
                kind: self.event.kind,
                title: &self.event.title,
                body: &self.event.body,
            },
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
    UnsupportedAction,
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

    fn owner(&self) -> Option<AgentOwner> {
        Some(AgentOwner {
            agent: self.agent.clone(),
            session_id: self.session_id.clone(),
            turn_id: self.turn_id.clone()?,
        })
    }
}

fn validate_identity(
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

#[derive(Clone, Debug)]
pub struct PendingCompletion {
    pub owner: AgentOwner,
    pub turn_generation: u64,
    pub deadline: Instant,
    title: String,
    body: String,
}

#[derive(Clone, Debug)]
pub struct AgentPaneState {
    pub current_owner: Option<AgentOwner>,
    pub turn_generation: u64,
    pub status: AgentRuntimeStatus,
    pub has_work_evidence: bool,
    pub state_started_at: Instant,
    pub updated_at: Instant,
    pub pending_completion: Option<PendingCompletion>,
    notification_generation: u64,
}

impl AgentPaneState {
    fn new(now: Instant) -> Self {
        Self {
            current_owner: None,
            turn_generation: 0,
            status: AgentRuntimeStatus::Idle,
            has_work_evidence: false,
            state_started_at: now,
            updated_at: now,
            pending_completion: None,
            notification_generation: 0,
        }
    }

    fn set_status(&mut self, status: AgentRuntimeStatus, now: Instant) -> bool {
        let changed = self.status != status;

        if changed {
            self.status = status;
            self.state_started_at = now;
        }

        self.updated_at = now;

        changed
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentNotification {
    pub id: String,
    pub route: AgentRoute,
    pub title: String,
    pub body: String,
    pub order: u64,
    pub read: bool,
    pub native_tag: String,
    pub native_group: String,
    pub native_requested: bool,
    native_after: Option<Instant>,
}

#[derive(Clone, Debug, Default)]
pub struct MonitorMutation {
    pub visible_changed: bool,
    pub created_notification: Option<AgentNotification>,
    pub removed_notifications: Vec<AgentNotification>,
}

impl MonitorMutation {
    fn merge(&mut self, mut other: Self) {
        self.visible_changed |= other.visible_changed;

        if other.created_notification.is_some() {
            self.created_notification = other.created_notification.take();
        }

        self.removed_notifications
            .append(&mut other.removed_notifications);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProjection {
    pub status: AgentRuntimeStatus,
    pub unread_count: usize,
    pub latest_unread_text: Option<String>,
}

pub struct AgentMonitor {
    process_instance: String,
    panes: HashMap<AgentRoute, AgentPaneState>,
    notifications: HashMap<AgentRoute, AgentNotification>,
    next_notification_order: u64,
}

impl AgentMonitor {
    pub fn new(process_instance: impl Into<String>) -> Self {
        Self {
            process_instance: process_instance.into(),
            panes: HashMap::new(),
            notifications: HashMap::new(),
            next_notification_order: 0,
        }
    }

    pub fn register_route(&mut self, route: AgentRoute, now: Instant) -> bool {
        if self.panes.contains_key(&route) {
            false
        } else {
            self.panes.insert(route, AgentPaneState::new(now));
            true
        }
    }

    #[cfg(test)]
    pub fn pane(&self, route: &AgentRoute) -> Option<&AgentPaneState> {
        self.panes.get(route)
    }

    pub fn notification(&self, route: &AgentRoute) -> Option<&AgentNotification> {
        self.notifications.get(route)
    }

    pub fn pending_native_notifications(&self, now: Instant) -> Vec<AgentNotification> {
        self.notifications
            .values()
            .filter(|notification| {
                !notification.read
                    && !notification.native_requested
                    && notification
                        .native_after
                        .is_none_or(|deadline| deadline <= now)
            })
            .cloned()
            .collect()
    }

    pub fn notifications(&self) -> Vec<AgentNotification> {
        self.notifications.values().cloned().collect()
    }

    pub fn mark_native_requested(&mut self, route: &AgentRoute, notification_id: &str) -> bool {
        let Some(notification) = self.notifications.get_mut(route) else {
            return false;
        };

        if notification.id != notification_id || notification.read || notification.native_requested
        {
            return false;
        }

        notification.native_requested = true;

        true
    }

    pub fn apply(&mut self, event: AgentEvent, now: Instant) -> MonitorMutation {
        if !self.panes.contains_key(&event.route) {
            return MonitorMutation::default();
        }

        let route = event.route.clone();

        match event.kind {
            AgentEventKind::SessionStarted => {
                let state = self.panes.get_mut(&route).expect("live route");

                // Session start alone is not evidence of a running turn; it
                // only refreshes pane liveness.
                if state.current_owner.is_none() || state.status == AgentRuntimeStatus::Idle {
                    state.updated_at = now;
                }

                MonitorMutation::default()
            }
            AgentEventKind::PromptSubmitted => {
                let owner = event.owner().expect("validated prompt has turn id");
                let state = self.panes.get_mut(&route).expect("live route");

                let same_turn = state.current_owner.as_ref() == Some(&owner);
                if !same_turn {
                    state.turn_generation = state.turn_generation.wrapping_add(1).max(1);
                }

                state.current_owner = Some(owner);
                state.has_work_evidence = true;
                state.pending_completion = None;

                let status_changed = state.set_status(AgentRuntimeStatus::Running, now);

                let mut mutation = self.remove_notification(&route);

                mutation.visible_changed |= status_changed || !same_turn;

                mutation
            }
            AgentEventKind::ToolStarted | AgentEventKind::ToolFinished => {
                let Some(owner) = event.owner() else {
                    return MonitorMutation::default();
                };

                let state = self.panes.get_mut(&route).expect("live route");
                if state.current_owner.as_ref() != Some(&owner) {
                    return MonitorMutation::default();
                }

                let codex_permission_resolved =
                    owner.agent == "codex" && state.status == AgentRuntimeStatus::NeedsInput;

                state.has_work_evidence = true;
                state.pending_completion = None;

                let visible_changed = state.set_status(AgentRuntimeStatus::Running, now);

                let mut mutation = if codex_permission_resolved {
                    self.remove_notification(&route)
                } else {
                    MonitorMutation::default()
                };

                mutation.visible_changed |= visible_changed;

                mutation
            }
            AgentEventKind::PermissionRequested => {
                let Some(owner) = event.owner() else {
                    return MonitorMutation::default();
                };

                let state = self.panes.get_mut(&route).expect("live route");
                if state.current_owner.as_ref() != Some(&owner) || !state.has_work_evidence {
                    return MonitorMutation::default();
                }

                state.pending_completion = None;

                let status_changed = state.set_status(AgentRuntimeStatus::NeedsInput, now);

                // Codex emits PermissionRequest before an automatic reviewer
                // settles, but its hook payload carries no final approval result.
                // Delay native delivery so same-turn tool progress can cancel
                // transient checks while unresolved requests still surface.
                let mut mutation = self.create_notification(
                    &route,
                    event.title,
                    event.body,
                    (event.agent == "codex").then_some(now + COMPLETION_QUIET_WINDOW),
                );

                mutation.visible_changed |= status_changed;

                mutation
            }
            AgentEventKind::Stopped => {
                let Some(owner) = event.owner() else {
                    return MonitorMutation::default();
                };

                let state = self.panes.get_mut(&route).expect("live route");
                if state.current_owner.as_ref() != Some(&owner)
                    || !state.has_work_evidence
                    || state.status == AgentRuntimeStatus::Idle
                {
                    return MonitorMutation::default();
                }

                let generation = state.turn_generation;

                if state.pending_completion.as_ref().is_none_or(|pending| {
                    pending.owner != owner || pending.turn_generation != generation
                }) {
                    state.pending_completion = Some(PendingCompletion {
                        owner,
                        turn_generation: generation,
                        deadline: now + COMPLETION_QUIET_WINDOW,
                        title: event.title,
                        body: event.body,
                    });
                }
                MonitorMutation::default()
            }
        }
    }

    pub fn process_due(&mut self, now: Instant) -> MonitorMutation {
        let routes: Vec<_> = self.panes.keys().cloned().collect();

        let mut result = MonitorMutation::default();

        for route in routes {
            let completion = self
                .panes
                .get(&route)
                .and_then(|state| state.pending_completion.clone())
                .filter(|pending| pending.deadline <= now);

            if let Some(pending) = completion {
                let commit = self.panes.get(&route).is_some_and(|state| {
                    state.current_owner.as_ref() == Some(&pending.owner)
                        && state.turn_generation == pending.turn_generation
                        && state.has_work_evidence
                        && state.status != AgentRuntimeStatus::Idle
                });

                let state = self.panes.get_mut(&route).expect("route still registered");

                state.pending_completion = None;

                if commit {
                    state.has_work_evidence = false;

                    let status_changed = state.set_status(AgentRuntimeStatus::Idle, now);

                    let mut mutation =
                        self.create_notification(&route, pending.title, pending.body, None);

                    mutation.visible_changed |= status_changed;

                    result.merge(mutation);
                }
            }

            let stale = self.panes.get(&route).is_some_and(|state| {
                matches!(
                    state.status,
                    AgentRuntimeStatus::Running | AgentRuntimeStatus::NeedsInput
                ) && state.updated_at + ACTIVE_STATE_STALE_AFTER <= now
            });

            if stale {
                let state = self.panes.get_mut(&route).expect("route still registered");

                state.pending_completion = None;
                state.has_work_evidence = false;

                result.visible_changed |= state.set_status(AgentRuntimeStatus::Idle, now);
            }
        }
        result
    }

    pub fn notify(&mut self, route: &AgentRoute, title: &str, body: &str) -> MonitorMutation {
        if !self.panes.contains_key(route) {
            return MonitorMutation::default();
        }

        self.create_notification(route, normalize_title(title), normalize_body(body), None)
    }

    pub fn interrupt(&mut self, route: &AgentRoute, now: Instant) -> MonitorMutation {
        let Some(state) = self.panes.get_mut(route) else {
            return MonitorMutation::default();
        };

        if state.status == AgentRuntimeStatus::Idle {
            return MonitorMutation::default();
        }

        state.current_owner = None;
        state.has_work_evidence = false;
        state.pending_completion = None;

        let status_changed = state.set_status(AgentRuntimeStatus::Idle, now);

        let mut mutation = self.remove_notification(route);

        mutation.visible_changed |= status_changed;

        mutation
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        let pane_deadline = self
            .panes
            .values()
            .flat_map(|state| {
                let completion = state.pending_completion.as_ref().map(|p| p.deadline);
                let stale = matches!(
                    state.status,
                    AgentRuntimeStatus::Running | AgentRuntimeStatus::NeedsInput
                )
                .then_some(state.updated_at + ACTIVE_STATE_STALE_AFTER);
                completion.into_iter().chain(stale)
            })
            .min();

        let native_deadline = self
            .notifications
            .values()
            .filter(|notification| !notification.read && !notification.native_requested)
            .filter_map(|notification| notification.native_after)
            .min();

        pane_deadline.into_iter().chain(native_deadline).min()
    }

    pub fn acknowledge(&mut self, route: &AgentRoute, notification_id: &str) -> MonitorMutation {
        let Some(notification) = self.notifications.get_mut(route) else {
            return MonitorMutation::default();
        };

        if notification.id != notification_id || notification.read {
            return MonitorMutation::default();
        }

        notification.read = true;

        MonitorMutation {
            visible_changed: true,
            removed_notifications: vec![notification.clone()],
            ..MonitorMutation::default()
        }
    }

    pub fn remove_route(&mut self, route: &AgentRoute) -> MonitorMutation {
        let removed_state = self.panes.remove(route).is_some();

        let mut mutation = self.remove_notification(route);

        mutation.visible_changed |= removed_state;

        mutation
    }

    pub fn project<'a>(&self, routes: impl IntoIterator<Item = &'a AgentRoute>) -> AgentProjection {
        let mut status = AgentRuntimeStatus::Idle;
        let mut unread_count = 0;
        let mut latest: Option<&AgentNotification> = None;

        for route in routes {
            if let Some(state) = self.panes.get(route) {
                status = higher_status(status, state.status);
            }

            if let Some(notification) = self.notifications.get(route).filter(|n| !n.read) {
                unread_count += 1;

                if latest.is_none_or(|current| notification.order > current.order) {
                    latest = Some(notification);
                }
            }
        }
        AgentProjection {
            status,
            unread_count,
            latest_unread_text: latest.map(|notification| {
                if notification.body.is_empty() {
                    notification.title.clone()
                } else {
                    notification.body.clone()
                }
            }),
        }
    }

    fn create_notification(
        &mut self,
        route: &AgentRoute,
        title: String,
        body: String,
        native_after: Option<Instant>,
    ) -> MonitorMutation {
        let state = self.panes.get_mut(route).expect("live route");

        state.notification_generation = state.notification_generation.wrapping_add(1).max(1);
        self.next_notification_order = self.next_notification_order.wrapping_add(1).max(1);

        let process_order = agent_process().next_notification_counter();

        let id = format!(
            "{}:{}:{}:{process_order}",
            self.process_instance, route.0, state.notification_generation,
        );

        let notification = AgentNotification {
            id: id.clone(),
            route: route.clone(),
            title,
            body,
            order: self.next_notification_order,
            read: false,
            native_tag: format!("{process_order:016x}"),
            native_group: "NiumaTerm".into(),
            native_requested: false,
            native_after,
        };

        let removed = self
            .notifications
            .insert(route.clone(), notification.clone());

        MonitorMutation {
            visible_changed: true,
            created_notification: Some(notification),
            removed_notifications: removed.into_iter().collect(),
        }
    }

    fn remove_notification(&mut self, route: &AgentRoute) -> MonitorMutation {
        let removed_notifications: Vec<_> = self.notifications.remove(route).into_iter().collect();

        MonitorMutation {
            visible_changed: !removed_notifications.is_empty(),
            removed_notifications,
            ..MonitorMutation::default()
        }
    }
}

fn higher_status(left: AgentRuntimeStatus, right: AgentRuntimeStatus) -> AgentRuntimeStatus {
    fn priority(status: AgentRuntimeStatus) -> u8 {
        match status {
            AgentRuntimeStatus::Idle => 0,
            AgentRuntimeStatus::Running => 1,
            AgentRuntimeStatus::NeedsInput => 2,
        }
    }

    if priority(right) > priority(left) {
        right
    } else {
        left
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, slice};

    use super::*;

    const TOKEN: &str = "hook-secret";

    fn route(value: &str) -> AgentRoute {
        AgentRoute::parse(value).unwrap()
    }

    #[test]
    fn process_routes_are_unique_and_environment_is_exact() {
        let process = AgentProcess::new();
        let first = process.allocate_route();
        let second = process.allocate_route();
        assert_ne!(first, second);
        let environment = process.environment_for(&first);
        assert_eq!(environment.len(), 3);
        assert_eq!(environment[0], (AGENT_ROUTE_ENV.into(), first.0.clone()));
        assert_eq!(
            environment[1],
            (AGENT_HOOK_TOKEN_ENV.into(), process.hook_token.clone())
        );
        assert_eq!(environment[2], (AGENT_HOOK_VERSION_ENV.into(), "1".into()));

        process.set_hook_executable("C:\\NiumaTerm\\NiumaTermHook.exe".into());
        process.set_hook_executable("C:\\ignored\\second\\call.exe".into());
        assert_eq!(
            process.environment_for(&first)[3],
            (
                AGENT_HOOK_EXE_ENV.into(),
                "C:\\NiumaTerm\\NiumaTermHook.exe".into()
            )
        );

        process.set_testing(true);
        assert_eq!(
            process.environment_for(&first)[4],
            (AGENT_TESTING_ENV.into(), "1".into())
        );
    }

    #[test]
    fn windows_hook_command_uses_bare_safe_path() {
        assert_eq!(
            build_windows_hook_command_for(
                r"C:\Soft\NiumaTerm\NiumaTermHook.exe",
                "codex",
                r"C:\Windows",
            )
            .unwrap(),
            r"C:\Soft\NiumaTerm\NiumaTermHook.exe codex"
        );
        assert!(
            build_windows_hook_command_for(r"C:\Hook.exe", "codex & whoami", r"C:\Windows")
                .is_err()
        );
    }

    #[test]
    fn windows_hook_command_encodes_unsafe_path() {
        let executable = r"C:\Program Files\Niuma'Term\%HOOK%^\NiumaTermHook.exe";
        let command = build_windows_hook_command_for(executable, "codex", r"D:\Windows").unwrap();
        assert!(command.starts_with(
            "D:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe -NoProfile \
             -ExecutionPolicy Bypass -EncodedCommand "
        ));
        assert!(!command.contains(executable));
        assert!(hook_command_contains(&command, "NiumaTermHook.exe"));

        let encoded = command.rsplit_once(' ').unwrap().1;
        let bytes = STANDARD.decode(encoded).unwrap();
        let units = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        assert_eq!(
            String::from_utf16(&units).unwrap(),
            r"& 'C:\Program Files\Niuma''Term\%HOOK%^\NiumaTermHook.exe' codex; exit $LASTEXITCODE"
        );

        #[cfg(windows)]
        {
            let dir = env::temp_dir().join(format!("nmt hook path % ^ {}", process::id()));
            fs::create_dir_all(&dir).unwrap();
            let script = dir.join("hook.cmd");
            fs::write(&script, "@echo hook-ran\r\n@exit /b 0\r\n").unwrap();
            let command = build_windows_hook_command(script.to_str().unwrap(), "codex").unwrap();
            for (shell, args) in [
                ("powershell.exe", vec!["-NoProfile", "-Command", &command]),
                ("cmd.exe", vec!["/d", "/c", &command]),
            ] {
                let output = process::Command::new(shell).args(args).output().unwrap();
                assert_eq!(
                    output.status.code(),
                    Some(0),
                    "{shell}: stdout={}, stderr={}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(String::from_utf8_lossy(&output.stdout).contains("hook-ran"));
            }
            fs::remove_dir_all(dir).unwrap();
        }
    }

    fn event(
        route: &AgentRoute,
        session: &str,
        turn: Option<&str>,
        kind: AgentEventKind,
    ) -> AgentEvent {
        AgentEvent::validate(
            AgentEventInput {
                route: route.as_str(),
                token: TOKEN,
                version: AGENT_HOOK_PROTOCOL_VERSION,
                agent: "codex",
                session_id: session,
                turn_id: turn,
                kind,
                title: "Codex",
                body: "Agent update",
            },
            TOKEN,
        )
        .unwrap()
    }

    fn monitor(now: Instant, routes: &[AgentRoute]) -> AgentMonitor {
        let mut monitor = AgentMonitor::new("process");
        for route in routes {
            assert!(monitor.register_route(route.clone(), now));
        }
        monitor
    }

    #[test]
    fn validation_is_strict_and_presentation_is_bounded() {
        let r = route("pane-1");
        let input = AgentEventInput {
            route: r.as_str(),
            token: "wrong",
            version: 1,
            agent: "codex",
            session_id: "session",
            turn_id: Some("turn"),
            kind: AgentEventKind::PromptSubmitted,
            title: "title",
            body: "body",
        };
        assert_eq!(
            AgentEvent::validate(input, TOKEN),
            Err(AgentValidationError::InvalidToken)
        );
        assert!(AgentRoute::parse("").is_err());
        assert!(AgentRoute::parse(&"x".repeat(MAX_ROUTE_BYTES + 1)).is_err());
        assert_eq!(normalize_title(" a\n\0  b "), "a b");
        assert_eq!(normalize_body("a\r\nb\0c"), "a\nb c");
        let unicode = "🦀".repeat(MAX_TITLE_CHARS + 1);
        let normalized = normalize_title(&unicode);
        assert_eq!(normalized.chars().count(), MAX_TITLE_CHARS);
        assert!(normalized.is_char_boundary(normalized.len()));
    }

    #[test]
    fn session_start_does_not_invent_running() {
        let now = Instant::now();
        let r = route("pane-1");
        let mut monitor = monitor(now, &[r.clone()]);
        monitor.apply(event(&r, "s1", None, AgentEventKind::SessionStarted), now);
        let state = monitor.pane(&r).unwrap();
        assert_eq!(state.status, AgentRuntimeStatus::Idle);
        assert_eq!(state.current_owner, None);
    }

    #[test]
    fn prompt_claims_owner_and_replay_does_not_advance_generation() {
        let now = Instant::now();
        let r = route("pane-1");
        let mut monitor = monitor(now, &[r.clone()]);
        let prompt = event(&r, "s1", Some("opaque-b"), AgentEventKind::PromptSubmitted);
        monitor.apply(prompt.clone(), now);
        monitor.apply(prompt, now + Duration::from_secs(1));
        let state = monitor.pane(&r).unwrap();
        assert_eq!(state.status, AgentRuntimeStatus::Running);
        assert_eq!(state.turn_generation, 1);
        assert!(state.has_work_evidence);
    }

    #[test]
    fn new_prompt_supersedes_needs_input_and_old_stop_is_ignored() {
        let now = Instant::now();
        let r = route("pane-1");
        let mut monitor = monitor(now, &[r.clone()]);
        monitor.apply(
            event(&r, "s1", Some("z"), AgentEventKind::PromptSubmitted),
            now,
        );
        monitor.apply(
            event(&r, "s1", Some("z"), AgentEventKind::PermissionRequested),
            now,
        );
        assert_eq!(
            monitor.pane(&r).unwrap().status,
            AgentRuntimeStatus::NeedsInput
        );
        assert!(monitor.notification(&r).is_some());
        monitor.apply(
            event(&r, "s2", Some("a"), AgentEventKind::PromptSubmitted),
            now,
        );
        monitor.apply(event(&r, "s1", Some("z"), AgentEventKind::Stopped), now);
        assert_eq!(
            monitor.pane(&r).unwrap().status,
            AgentRuntimeStatus::Running
        );
        assert_eq!(monitor.pane(&r).unwrap().turn_generation, 2);
        assert!(monitor.notification(&r).is_none());
        assert!(monitor.pane(&r).unwrap().pending_completion.is_none());
    }

    #[test]
    fn permission_notification_waits_and_auto_approval_cancels_it() {
        let now = Instant::now();
        let auto = route("auto");
        let waiting = route("waiting");
        let mut monitor = monitor(now, &[auto.clone(), waiting.clone()]);
        for route in [&auto, &waiting] {
            monitor.apply(
                event(route, "s", Some("t"), AgentEventKind::PromptSubmitted),
                now,
            );
            monitor.apply(
                event(route, "s", Some("t"), AgentEventKind::PermissionRequested),
                now,
            );
            assert!(monitor.notification(route).is_some());
        }
        assert!(monitor.pending_native_notifications(now).is_empty());

        monitor.apply(
            event(&auto, "s", Some("t"), AgentEventKind::ToolFinished),
            now + Duration::from_millis(100),
        );
        monitor.process_due(now + COMPLETION_QUIET_WINDOW);

        assert!(monitor.notification(&auto).is_none());
        assert!(monitor.notification(&waiting).is_some());
        assert_eq!(
            monitor.pending_native_notifications(now + COMPLETION_QUIET_WINDOW),
            vec![monitor.notification(&waiting).unwrap().clone()]
        );
        assert_eq!(
            monitor.pane(&auto).unwrap().status,
            AgentRuntimeStatus::Running
        );
        assert_eq!(
            monitor.pane(&waiting).unwrap().status,
            AgentRuntimeStatus::NeedsInput
        );
    }

    #[test]
    fn nested_session_and_opaque_turn_events_cannot_steal_owner() {
        let now = Instant::now();
        let r = route("pane-1");
        let mut monitor = monitor(now, &[r.clone()]);
        monitor.apply(
            event(&r, "parent", Some("10"), AgentEventKind::PromptSubmitted),
            now,
        );
        monitor.apply(
            event(&r, "child", None, AgentEventKind::SessionStarted),
            now,
        );
        monitor.apply(
            event(&r, "parent", Some("2"), AgentEventKind::ToolFinished),
            now,
        );
        monitor.apply(event(&r, "child", Some("1"), AgentEventKind::Stopped), now);
        let owner = monitor.pane(&r).unwrap().current_owner.as_ref().unwrap();
        assert_eq!(owner.session_id, "parent");
        assert_eq!(owner.turn_id, "10");
        assert!(monitor.pane(&r).unwrap().pending_completion.is_none());
    }

    #[test]
    fn stop_quiets_then_commits_once_and_resumed_work_cancels() {
        let now = Instant::now();
        let r = route("pane-1");
        let mut monitor = monitor(now, &[r.clone()]);
        monitor.apply(
            event(&r, "s", Some("t"), AgentEventKind::PromptSubmitted),
            now,
        );
        let stop = event(&r, "s", Some("t"), AgentEventKind::Stopped);
        monitor.apply(stop.clone(), now);
        monitor.apply(stop, now);
        assert_eq!(monitor.next_deadline(), Some(now + COMPLETION_QUIET_WINDOW));
        monitor.process_due(now + COMPLETION_QUIET_WINDOW - Duration::from_millis(1));
        assert_eq!(
            monitor.pane(&r).unwrap().status,
            AgentRuntimeStatus::Running
        );
        monitor.apply(event(&r, "s", Some("t"), AgentEventKind::ToolStarted), now);
        assert!(monitor.pane(&r).unwrap().pending_completion.is_none());
        monitor.apply(event(&r, "s", Some("t"), AgentEventKind::Stopped), now);
        monitor.process_due(now + COMPLETION_QUIET_WINDOW);
        let first_id = monitor.notification(&r).unwrap().id.clone();
        assert_eq!(monitor.pane(&r).unwrap().status, AgentRuntimeStatus::Idle);
        monitor.apply(event(&r, "s", Some("t"), AgentEventKind::Stopped), now);
        monitor.process_due(now + COMPLETION_QUIET_WINDOW * 2);
        assert_eq!(monitor.notification(&r).unwrap().id, first_id);
    }

    #[test]
    fn stop_without_current_runtime_evidence_never_notifies() {
        let now = Instant::now();
        let r = route("pane-1");
        let mut monitor = monitor(now, &[r.clone()]);
        monitor.apply(event(&r, "s", None, AgentEventKind::SessionStarted), now);
        monitor.apply(event(&r, "s", Some("t"), AgentEventKind::Stopped), now);
        monitor.process_due(now + COMPLETION_QUIET_WINDOW);
        assert!(monitor.notification(&r).is_none());
    }

    #[test]
    fn stale_active_state_becomes_idle_without_notification() {
        let now = Instant::now();
        let r = route("pane-1");
        let mut monitor = monitor(now, &[r.clone()]);
        monitor.apply(
            event(&r, "s", Some("t"), AgentEventKind::PromptSubmitted),
            now,
        );
        assert_eq!(
            monitor.next_deadline(),
            Some(now + ACTIVE_STATE_STALE_AFTER)
        );
        monitor.process_due(now + ACTIVE_STATE_STALE_AFTER);
        assert_eq!(monitor.pane(&r).unwrap().status, AgentRuntimeStatus::Idle);
        assert!(monitor.notification(&r).is_none());
    }

    #[test]
    fn matching_update_reschedules_stale_expiry() {
        let now = Instant::now();
        let update = now + Duration::from_secs(60);
        let r = route("pane-1");
        let mut monitor = monitor(now, &[r.clone()]);
        monitor.apply(
            event(&r, "s", Some("t"), AgentEventKind::PromptSubmitted),
            now,
        );
        monitor.apply(
            event(&r, "s", Some("t"), AgentEventKind::ToolFinished),
            update,
        );
        assert_eq!(
            monitor.next_deadline(),
            Some(update + ACTIVE_STATE_STALE_AFTER)
        );
        monitor.process_due(now + ACTIVE_STATE_STALE_AFTER);
        assert_eq!(
            monitor.pane(&r).unwrap().status,
            AgentRuntimeStatus::Running
        );
        monitor.process_due(update + ACTIVE_STATE_STALE_AFTER);
        assert_eq!(monitor.pane(&r).unwrap().status, AgentRuntimeStatus::Idle);
    }

    #[test]
    fn old_generation_completion_timer_cannot_complete_new_prompt() {
        let now = Instant::now();
        let r = route("pane-1");
        let mut monitor = monitor(now, &[r.clone()]);
        monitor.apply(
            event(&r, "s", Some("old"), AgentEventKind::PromptSubmitted),
            now,
        );
        monitor.apply(event(&r, "s", Some("old"), AgentEventKind::Stopped), now);
        monitor.apply(
            event(&r, "s", Some("new"), AgentEventKind::PromptSubmitted),
            now + Duration::from_millis(100),
        );
        monitor.process_due(now + COMPLETION_QUIET_WINDOW);
        assert_eq!(
            monitor.pane(&r).unwrap().status,
            AgentRuntimeStatus::Running
        );
        assert!(monitor.notification(&r).is_none());
    }

    #[test]
    fn latest_notification_acknowledgement_and_status_are_independent() {
        let now = Instant::now();
        let r = route("pane-1");
        let mut monitor = monitor(now, &[r.clone()]);
        monitor.apply(
            event(&r, "s", Some("t"), AgentEventKind::PromptSubmitted),
            now,
        );
        monitor.apply(
            event(&r, "s", Some("t"), AgentEventKind::PermissionRequested),
            now,
        );
        let old = monitor.notification(&r).unwrap().id.clone();
        monitor.apply(
            event(&r, "s", Some("t"), AgentEventKind::PermissionRequested),
            now,
        );
        let current = monitor.notification(&r).unwrap().id.clone();
        assert_eq!(monitor.notification(&r).unwrap().native_tag.len(), 16);
        assert_eq!(monitor.notification(&r).unwrap().native_group, "NiumaTerm");
        assert_ne!(old, current);
        assert_eq!(
            monitor
                .pending_native_notifications(now + COMPLETION_QUIET_WINDOW)
                .len(),
            1
        );
        assert!(monitor.mark_native_requested(&r, &current));
        assert!(
            monitor
                .pending_native_notifications(now + COMPLETION_QUIET_WINDOW)
                .is_empty()
        );
        assert!(!monitor.mark_native_requested(&r, &old));
        assert!(!monitor.acknowledge(&r, &old).visible_changed);
        assert!(monitor.acknowledge(&r, &current).visible_changed);
        assert!(monitor.notification(&r).unwrap().read);
        assert_eq!(
            monitor.pane(&r).unwrap().status,
            AgentRuntimeStatus::NeedsInput
        );
    }

    #[test]
    fn failed_native_operations_cannot_clear_internal_attention() {
        let now = Instant::now();
        let r = route("pane-1");
        let mut monitor = monitor(now, &[r.clone()]);
        monitor.apply(
            event(&r, "s", Some("t"), AgentEventKind::PromptSubmitted),
            now,
        );
        monitor.apply(
            event(&r, "s", Some("t"), AgentEventKind::PermissionRequested),
            now,
        );
        let id = monitor.notification(&r).unwrap().id.clone();

        // Native delivery is intentionally fire-and-forget. Recording a failed
        // attempt changes only its retry marker, never the internal projection.
        assert!(monitor.mark_native_requested(&r, &id));
        assert_eq!(monitor.project([&r]).status, AgentRuntimeStatus::NeedsInput);
        assert_eq!(monitor.project([&r]).unread_count, 1);

        // Native removal failure is likewise unable to undo the exact internal
        // acknowledgement or mutate the agent lifecycle state.
        assert!(monitor.acknowledge(&r, &id).visible_changed);
        assert_eq!(monitor.project([&r]).unread_count, 0);
        assert_eq!(monitor.project([&r]).status, AgentRuntimeStatus::NeedsInput);
    }

    #[test]
    fn native_delivery_is_suppressed_only_for_the_exact_visible_route() {
        let target = route("target");
        let sibling = route("sibling");
        assert!(!request_native_delivery(Some(&target), &target));
        assert!(request_native_delivery(Some(&sibling), &target));
        assert!(request_native_delivery(None, &target));
    }

    #[test]
    fn stale_gpui_active_flag_does_not_treat_minimized_window_as_visible() {
        assert!(exact_window_is_active(true, true, false));
        assert!(!exact_window_is_active(true, true, true));
        assert!(!exact_window_is_active(true, false, false));
        assert!(!exact_window_is_active(false, true, false));
    }

    #[test]
    fn aggregation_counts_routes_and_prioritizes_needs_input() {
        let now = Instant::now();
        let a = route("a");
        let b = route("b");
        let mut monitor = monitor(now, &[a.clone(), b.clone()]);
        assert_eq!(monitor.project([&a, &b]).status, AgentRuntimeStatus::Idle);
        monitor.apply(
            event(&a, "s1", Some("t1"), AgentEventKind::PromptSubmitted),
            now,
        );
        monitor.apply(
            event(&b, "s2", Some("t2"), AgentEventKind::PromptSubmitted),
            now,
        );
        assert_eq!(
            monitor.project([&a, &b]).status,
            AgentRuntimeStatus::Running
        );
        monitor.apply(
            event(&a, "s1", Some("t1"), AgentEventKind::PermissionRequested),
            now,
        );
        monitor.apply(
            event(&b, "s2", Some("t2"), AgentEventKind::PermissionRequested),
            now,
        );
        let projection = monitor.project([&a, &b]);
        assert_eq!(projection.status, AgentRuntimeStatus::NeedsInput);
        assert_eq!(projection.unread_count, 2);
        assert_eq!(
            projection.latest_unread_text.as_deref(),
            Some("Agent update")
        );
        monitor.remove_route(&a);
        assert_eq!(monitor.project([&a, &b]).unread_count, 1);
    }

    #[test]
    fn tab_activation_keeps_split_sibling_unread_until_exact_acknowledgement() {
        let now = Instant::now();
        let tab_one_left = route("tab-1:left");
        let tab_one_right = route("tab-1:right");
        let tab_two = route("tab-2:only");
        let mut monitor = monitor(
            now,
            &[tab_one_left.clone(), tab_one_right.clone(), tab_two.clone()],
        );
        monitor.notify(&tab_one_left, "left", "left unread");
        monitor.notify(&tab_one_right, "right", "right unread");
        monitor.notify(&tab_two, "second tab", "latest unread");

        let tab_one = monitor.project([&tab_one_left, &tab_one_right]);
        let tab_two_projection = monitor.project([&tab_two]);
        let workspace = monitor.project([&tab_one_left, &tab_one_right, &tab_two]);
        assert_eq!(tab_one.unread_count, 2);
        assert_eq!(tab_two_projection.unread_count, 1);
        assert_eq!(workspace.unread_count, 3);
        assert_eq!(
            workspace.latest_unread_text.as_deref(),
            Some("latest unread")
        );

        let left_id = monitor.notification(&tab_one_left).unwrap().id.clone();
        monitor.acknowledge(&tab_one_left, &left_id);
        assert_eq!(
            monitor
                .project([&tab_one_left, &tab_one_right])
                .unread_count,
            1
        );
        assert_eq!(
            monitor
                .project([&tab_one_left, &tab_one_right, &tab_two])
                .unread_count,
            2
        );

        let right_id = monitor.notification(&tab_one_right).unwrap().id.clone();
        monitor.acknowledge(&tab_one_right, &right_id);
        assert_eq!(
            monitor
                .project([&tab_one_left, &tab_one_right])
                .unread_count,
            0
        );
        assert_eq!(
            monitor
                .project([&tab_one_left, &tab_one_right, &tab_two])
                .unread_count,
            1
        );
    }

    #[test]
    fn osc_style_notification_replaces_latest_without_changing_agent_state() {
        let now = Instant::now();
        let r = route("pane-1");
        let other = route("pane-2");
        let mut monitor = monitor(now, &[r.clone(), other.clone()]);
        monitor.apply(
            event(&r, "s", Some("t"), AgentEventKind::PromptSubmitted),
            now,
        );
        monitor.notify(&r, "first", "old");
        let old = monitor.notification(&r).unwrap().id.clone();
        let mutation = monitor.notify(&r, &"🦀".repeat(300), &"b".repeat(5_000));
        assert_eq!(mutation.removed_notifications[0].id, old);
        assert_eq!(
            monitor.pane(&r).unwrap().status,
            AgentRuntimeStatus::Running
        );
        assert_eq!(monitor.notification(&r).unwrap().title.chars().count(), 256);
        assert_eq!(
            monitor.notification(&r).unwrap().body.chars().count(),
            4_096
        );
        monitor.notify(&other, "other", "separate unread");
        assert_eq!(monitor.project([&r, &other]).unread_count, 2);
        assert_eq!(
            monitor
                .pane(&r)
                .unwrap()
                .current_owner
                .as_ref()
                .unwrap()
                .session_id,
            "s"
        );
    }

    #[test]
    fn closed_route_cancels_pending_and_rejects_late_events() {
        let now = Instant::now();
        let r = route("pane-1");
        let mut monitor = monitor(now, &[r.clone()]);
        monitor.apply(
            event(&r, "s", Some("t"), AgentEventKind::PromptSubmitted),
            now,
        );
        monitor.apply(event(&r, "s", Some("t"), AgentEventKind::Stopped), now);
        monitor.remove_route(&r);
        monitor.process_due(now + COMPLETION_QUIET_WINDOW);
        monitor.apply(
            event(&r, "s", Some("t"), AgentEventKind::PromptSubmitted),
            now,
        );
        assert!(monitor.pane(&r).is_none());
        assert!(monitor.notification(&r).is_none());
    }

    #[test]
    fn colliding_local_pane_ids_stay_isolated_across_windows_and_close_cascades() {
        let now = Instant::now();
        let local_pane_id = 1;
        let window_one = route("window-1:route-1");
        let window_two = route("window-2:route-1");
        assert_eq!(local_pane_id, 1); // Both windows may independently allocate pane 1.

        let mut first = monitor(now, slice::from_ref(&window_one));
        let mut second = monitor(now, slice::from_ref(&window_two));
        let second_event = event(
            &window_two,
            "session-2",
            Some("turn-2"),
            AgentEventKind::PromptSubmitted,
        );
        assert!(!first.apply(second_event.clone(), now).visible_changed);
        assert!(second.apply(second_event, now).visible_changed);
        assert_eq!(
            first.project([&window_one]).status,
            AgentRuntimeStatus::Idle
        );
        assert_eq!(
            second.project([&window_two]).status,
            AgentRuntimeStatus::Running
        );

        second.apply(
            event(
                &window_two,
                "session-2",
                Some("turn-2"),
                AgentEventKind::Stopped,
            ),
            now,
        );
        second.remove_route(&window_two); // pane/tab/workspace/window teardown converges here.
        second.process_due(now + COMPLETION_QUIET_WINDOW);
        assert!(second.pane(&window_two).is_none());
        assert!(second.notification(&window_two).is_none());
    }

    #[test]
    fn background_window_notification_activation_is_exact() {
        let now = Instant::now();
        let foreground = route("window-1:route-1");
        let background = route("window-2:route-1");
        let mut foreground_monitor = monitor(now, slice::from_ref(&foreground));
        let mut background_monitor = monitor(now, slice::from_ref(&background));

        foreground_monitor.notify(&foreground, "foreground", "leave unread");
        background_monitor.notify(&background, "background", "activate me");
        let notification = background_monitor
            .notification(&background)
            .unwrap()
            .clone();
        assert!(request_native_delivery(Some(&foreground), &background));
        assert!(background_monitor.mark_native_requested(&background, &notification.id));

        assert!(
            !foreground_monitor
                .acknowledge(&background, &notification.id)
                .visible_changed
        );
        assert!(
            background_monitor
                .acknowledge(&background, &notification.id)
                .visible_changed
        );
        assert_eq!(foreground_monitor.project([&foreground]).unread_count, 1);
        assert_eq!(background_monitor.project([&background]).unread_count, 0);
    }
}
