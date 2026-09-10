pub mod background_task;
pub mod chat;
pub mod claude_code;
pub mod codex;
pub mod deepseek;
pub mod git;
pub mod launcher;
pub mod message_memory;
pub mod session;
pub mod update;
pub mod usage;
pub mod workflow;
pub mod workspace;

mod deadline_timer;
mod hook_store;
mod request_policy;
mod subprocess;

mod event;
mod hook_command;
mod json;
mod monitor;
mod process;

pub use crate::codex::ProviderConfig as CodexProviderConfig;
#[cfg(test)]
use crate::event::MAX_TITLE_CHARS;
pub use crate::event::{
    AgentEvent, AgentEventInput, AgentEventKind, AgentOwner, AgentRuntimeStatus,
    AgentValidationError, RawAgentHookMessage, normalize_body, normalize_title,
};
use crate::event::{MAX_ROUTE_BYTES, validate_identity};
pub use crate::hook_command::{HookInstallStatus, build_hook_command, hook_command_contains};
pub use crate::monitor::{
    ACTIVE_STATE_STALE_AFTER, AgentActivityPolicy, AgentMonitor, AgentNotification, AgentPaneState,
    AgentProjection, COMPLETION_QUIET_WINDOW, MonitorMutation, PendingCompletion,
    request_native_delivery,
};
pub use crate::process::{
    AGENT_HOOK_EXE_ENV, AGENT_HOOK_PROTOCOL_VERSION, AGENT_HOOK_TOKEN_ENV, AGENT_HOOK_VERSION_ENV,
    AGENT_ROUTE_ENV, AGENT_TESTING_ENV, AgentProcess, agent_process,
};
pub use crate::workspace::{AgentWorkspace, MultiRootAccess};

/// How to launch an agent CLI. Protocol-specific settings are carried here so
/// adapters can map them onto their native environment or RPC surfaces.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchConfig {
    pub executable: String,
    /// Arguments belonging to the executable itself, ahead of whatever the
    /// adapter passes. A harness launched through a package runner takes its
    /// package name here, where it stays out of the command being run.
    pub executable_args: Vec<String>,
    pub model: Option<String>,
    /// Reasoning effort the profile pins for every conversation it starts.
    /// `None` leaves the level to the agent and the remembered thread
    /// settings. Each adapter maps it to its own surface.
    pub effort: Option<String>,
    pub provider: Option<CodexProviderConfig>,
    pub env: Vec<(String, String)>,
    /// Declare [`Self::model`] as an image-capable model in the harness's own
    /// provider catalog when a conversation starts. Only DeepSeek Harness has
    /// such a catalog: it refuses an image unless the selected model is listed
    /// there as taking one, and a model named by hand is never listed.
    pub declares_image_input: bool,
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
