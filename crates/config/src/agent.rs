//! Agent settings persisted as the `[agent]` section of `config.toml`.

use serde::{Deserialize, Deserializer, Serialize};
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
    /// How much of an agent tab's transcript arrives folded.
    #[serde(
        default,
        rename = "collapse-tool-calls",
        deserialize_with = "deserialize_collapse_rows"
    )]
    pub collapse_tool_calls: CollapseRows,
    /// Probe each Agent installation for a newer provider version in the
    /// background. Manual checks stay available while this is off.
    #[serde(default = "default_bool_true", rename = "check-agent-updates")]
    pub check_agent_updates: bool,
    /// List Codex skills in the `/` command palette and rewrite a chosen one
    /// to its `$name` form. With this off the `/` palette carries commands
    /// only and `$` is the sole skill trigger.
    #[serde(default = "default_bool_true", rename = "codex-skill-command-compat")]
    pub codex_skill_command_compat: bool,
    /// How the composer's model picker spells each model it offers.
    #[serde(default, rename = "model-list-style")]
    pub model_list_style: ModelListStyle,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enable_agent_hooks: true,
            show_agent_usage: true,
            collapse_tool_calls: CollapseRows::WorkAndToolCalls,
            check_agent_updates: true,
            codex_skill_command_compat: true,
            model_list_style: ModelListStyle::default(),
        }
    }
}

/// How the composer's model picker spells a model. A harness reports two
/// names for one model - the one it displays and the route id a pick is sent
/// as - and which of them identifies the model to the reader depends on the
/// deployment: display names repeat across snapshots of the same model, while
/// ids carry the snapshot date and the provider prefix.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ModelListStyle {
    #[default]
    NameAndId,
    IdAndName,
    NameOnly,
    IdOnly,
}

impl ModelListStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NameAndId => "name-and-id",
            Self::IdAndName => "id-and-name",
            Self::NameOnly => "name-only",
            Self::IdOnly => "id-only",
        }
    }

    pub fn from_value(value: &str) -> Self {
        match value {
            "id-and-name" => Self::IdAndName,
            "name-only" => Self::NameOnly,
            "id-only" => Self::IdOnly,
            _ => Self::NameAndId,
        }
    }

    /// One picker entry, from the harness's display name and the route id it
    /// is selected by. A harness that reports the id as the display name, or
    /// no name at all, leaves nothing to put beside the id, so the paired
    /// styles print the id once instead of twice.
    pub fn label(self, name: &str, id: &str) -> String {
        let name = name.trim();
        if name.is_empty() || name == id {
            return id.to_string();
        }

        match self {
            Self::NameAndId => format!("{name} ({id})"),
            Self::IdAndName => format!("{id} ({name})"),
            Self::NameOnly => name.to_string(),
            Self::IdOnly => id.to_string(),
        }
    }
}

/// How much of an agent tab's transcript arrives folded. Two independent
/// disclosures answer to this: a finished turn's work, and a run of
/// consecutive tool calls within whatever is on screen.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CollapseRows {
    /// Both: a finished turn keeps only its prompt and its answer, and runs
    /// of tool calls collapse to their newest.
    #[default]
    WorkAndToolCalls,
    /// Runs of tool calls only; a finished turn shows the work it did.
    ToolCalls,
    /// Neither; every row arrives on screen.
    Off,
}

impl CollapseRows {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WorkAndToolCalls => "work-and-tool-calls",
            Self::ToolCalls => "tool-calls",
            Self::Off => "off",
        }
    }

    pub fn from_value(value: &str) -> Self {
        match value {
            "tool-calls" => Self::ToolCalls,
            "off" => Self::Off,
            _ => Self::WorkAndToolCalls,
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CollapseRowsValue {
    Mode(CollapseRows),
    Legacy(bool),
}

fn deserialize_collapse_rows<'de, D>(deserializer: D) -> Result<CollapseRows, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match CollapseRowsValue::deserialize(deserializer)? {
        CollapseRowsValue::Mode(mode) => mode,
        // Legacy `collapse-tool-calls` boolean: it governed the tool-call runs
        // alone, while a finished turn folded its work whatever it said. Off
        // therefore describes a combination these modes do not offer, so both
        // values land on the one that keeps the turn fold the config was
        // written under.
        CollapseRowsValue::Legacy(true) => CollapseRows::WorkAndToolCalls,
        CollapseRowsValue::Legacy(false) => CollapseRows::WorkAndToolCalls,
    })
}

pub(crate) fn patch_document(doc: &mut DocumentMut, agent: &AgentConfig) {
    doc["agent"]["enable-agent-hooks"] = value(agent.enable_agent_hooks);
    doc["agent"]["show-agent-usage"] = value(agent.show_agent_usage);
    doc["agent"]["collapse-tool-calls"] = value(agent.collapse_tool_calls.as_str());
    doc["agent"]["check-agent-updates"] = value(agent.check_agent_updates);
    doc["agent"]["codex-skill-command-compat"] = value(agent.codex_skill_command_compat);
    doc["agent"]["model-list-style"] = value(agent.model_list_style.as_str());
}
