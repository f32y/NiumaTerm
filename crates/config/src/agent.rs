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
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enable_agent_hooks: true,
            show_agent_usage: true,
        }
    }
}

pub(crate) fn patch_document(doc: &mut DocumentMut, agent: &AgentConfig) {
    doc["agent"]["enable-agent-hooks"] = value(agent.enable_agent_hooks);
    doc["agent"]["show-agent-usage"] = value(agent.show_agent_usage);
}
