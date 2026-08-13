//! Agent settings persisted as the `[agent]` section of `config.toml`.

use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, value};

use crate::defaults::default_bool_true;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentConfig {
    /// Accept lifecycle events delivered by installed Agent hooks.
    #[serde(default = "default_bool_true", rename = "enable-agent-hooks")]
    pub enable_agent_hooks: bool,
    /// Show Agent account usage in the workspace sidebar.
    #[serde(default = "default_bool_true", rename = "show-agent-usage")]
    pub show_agent_usage: bool,
    /// Collapse consecutive tool-call rows in agent tabs into a one-line
    /// summary by default.
    #[serde(default, rename = "collapse-tool-calls")]
    pub collapse_tool_calls: bool,
    /// Probe each Agent installation for a newer provider version in the
    /// background. Manual checks stay available while this is off.
    #[serde(default = "default_bool_true", rename = "check-agent-updates")]
    pub check_agent_updates: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enable_agent_hooks: true,
            show_agent_usage: true,
            collapse_tool_calls: false,
            check_agent_updates: true,
        }
    }
}

pub(crate) fn patch_document(doc: &mut DocumentMut, agent: &AgentConfig) {
    doc["agent"]["enable-agent-hooks"] = value(agent.enable_agent_hooks);
    doc["agent"]["show-agent-usage"] = value(agent.show_agent_usage);
    doc["agent"]["collapse-tool-calls"] = value(agent.collapse_tool_calls);
    doc["agent"]["check-agent-updates"] = value(agent.check_agent_updates);
}
