pub use nmt_agent::profile::{ANTHROPIC_MODEL_ENV, agent_launch, launch_env_value};
pub use nmt_agent::session::AgentKind;

#[cfg(test)]
mod agent_profile_launch_tests;

use std::collections::BTreeMap;

use gpui::{Global, SharedString};
use gpui_component::{Icon, IconNamed};
use nmt_agent::chat::ThreadSettings;
#[cfg(test)]
use nmt_agent::profile::{
    ANTHROPIC_SUB_MODEL_ENVS, CODEX_CREDENTIAL_ENV_PREFIX, DEEPSEEK_API_KEY_ENV,
    DEEPSEEK_BASE_URL_ENV, OPENAI_API_KEY_ENV,
};
use nmt_agent::session::settings::RememberedSettings;
use nmt_config::local_state::AgentDefaults as StoredAgentDefaults;

/// Provider icons, defined beside the kind they mark. The usage view and the
/// settings chrome borrow them from here.
pub struct CodexIcon;

impl IconNamed for CodexIcon {
    fn path(self) -> SharedString {
        "icons/codex.svg".into()
    }
}

pub struct ClaudeIcon;

impl IconNamed for ClaudeIcon {
    fn path(self) -> SharedString {
        "icons/claude.svg".into()
    }
}

pub struct DeepSeekIcon;

impl IconNamed for DeepSeekIcon {
    fn path(self) -> SharedString {
        "icons/deepseek.svg".into()
    }
}

/// Application configuration and presentation for an agent kind.
pub trait AgentKindExt {
    fn icon(self) -> Icon;
}

impl AgentKindExt for AgentKind {
    /// The harness's own mark. Tabs and the profile list read it from here so
    /// one kind cannot end up wearing another's glyph in one of them.
    fn icon(self) -> Icon {
        match self {
            AgentKind::Codex => Icon::new(CodexIcon),
            AgentKind::Claude => Icon::new(ClaudeIcon),
            AgentKind::DeepSeek => Icon::new(DeepSeekIcon),
        }
    }
}

/// Last-chosen thread settings per agent profile name (agent ID for
/// entries written by older builds), seeding the dropdowns of newly opened
/// conversations, resumed Claude conversations, and the reviewer of resumed
/// Codex threads. Loaded from local_state.toml at startup, saved after user
/// changes, and included in the final quit snapshot.
#[derive(Default)]
pub struct AgentThreadDefaults(pub(super) RememberedSettings);

impl Global for AgentThreadDefaults {}

impl From<&BTreeMap<String, StoredAgentDefaults>> for AgentThreadDefaults {
    fn from(stored: &BTreeMap<String, StoredAgentDefaults>) -> Self {
        Self(RememberedSettings::from_entries(stored.iter().map(
            |(kind, d)| {
                (
                    kind.clone(),
                    ThreadSettings {
                        model: d.model.clone(),
                        approval: d.approval.clone(),
                        approvals_reviewer: d.approvals_reviewer.clone(),
                        sandbox: d.sandbox.clone(),
                        effort: d.effort.clone(),
                        tier: d.tier.clone(),
                    },
                )
            },
        )))
    }
}

impl From<&AgentThreadDefaults> for BTreeMap<String, StoredAgentDefaults> {
    fn from(value: &AgentThreadDefaults) -> Self {
        value
            .0
            .iter()
            .map(|(kind, s)| {
                (
                    kind.clone(),
                    StoredAgentDefaults {
                        model: s.model.clone(),
                        approval: s.approval.clone(),
                        approvals_reviewer: s.approvals_reviewer.clone(),
                        sandbox: s.sandbox.clone(),
                        effort: s.effort.clone(),
                        tier: s.tier.clone(),
                    },
                )
            })
            .collect()
    }
}
