pub mod background_task;
pub mod chat;
pub mod claude_code;
pub mod codex;
#[cfg(target_os = "windows")]
pub mod launcher;
#[cfg(target_os = "windows")]
pub mod update;
pub mod usage;
pub mod workflow;

mod hook_store;
#[cfg(target_os = "windows")]
mod subprocess;

mod event;
mod hook_command;
mod monitor;
mod process;

pub use codex::ProviderConfig as CodexProviderConfig;

#[cfg(test)]
use crate::event::MAX_TITLE_CHARS;
pub use crate::event::{
    AgentEvent, AgentEventInput, AgentEventKind, AgentOwner, AgentRuntimeStatus,
    AgentValidationError, RawAgentHookMessage, normalize_body, normalize_title,
};
use crate::event::{MAX_ROUTE_BYTES, validate_identity};
#[cfg(test)]
use crate::hook_command::build_windows_hook_command_for;
pub use crate::hook_command::{
    HookInstallStatus, build_windows_hook_command, hook_command_contains,
};
pub use crate::monitor::{
    ACTIVE_STATE_STALE_AFTER, AgentMonitor, AgentNotification, AgentPaneState, AgentProjection,
    COMPLETION_QUIET_WINDOW, MonitorMutation, PendingCompletion, exact_window_is_active,
    request_native_delivery,
};
pub use crate::process::{
    AGENT_HOOK_EXE_ENV, AGENT_HOOK_PROTOCOL_VERSION, AGENT_HOOK_TOKEN_ENV, AGENT_HOOK_VERSION_ENV,
    AGENT_ROUTE_ENV, AGENT_TESTING_ENV, AgentProcess, agent_process,
};

/// How to launch an agent CLI. Protocol-specific settings are carried here so
/// adapters can map them onto their native environment or RPC surfaces.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchConfig {
    pub executable: String,
    pub model: Option<String>,
    /// Reasoning effort the profile pins for every conversation it starts.
    /// `None` leaves the level to the agent and the remembered thread
    /// settings. Each adapter maps it to its own surface.
    pub effort: Option<String>,
    pub provider: Option<CodexProviderConfig>,
    pub env: Vec<(String, String)>,
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

#[cfg(test)]
mod tests;
