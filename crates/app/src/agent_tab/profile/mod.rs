pub use nmt_agent::profile::{ANTHROPIC_MODEL_ENV, agent_launch, launch_env_value};
pub use nmt_agent::session::AgentKind;

#[cfg(test)]
mod agent_profile_launch_tests;

use gpui::SharedString;
use gpui_component::{Icon, IconNamed};
use nmt_agent::chat::ThreadSettings;
#[cfg(test)]
use nmt_agent::profile::{
    ANTHROPIC_SUB_MODEL_ENVS, CODEX_CREDENTIAL_ENV_PREFIX, DEEPSEEK_API_KEY_ENV,
    DEEPSEEK_BASE_URL_ENV, OPENAI_API_KEY_ENV,
};
use nmt_config::local_state::AgentTabSettings;

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

/// The stored shape of one tab's thread controls, as the live shape the
/// session runs on. The two stay separate types: the stored one is a file
/// format that has to survive changes to the controls this UI offers.
pub fn thread_settings_from_saved(saved: &AgentTabSettings) -> ThreadSettings {
    ThreadSettings {
        model: saved.model.clone(),
        approval: saved.approval.clone(),
        approvals_reviewer: saved.approvals_reviewer.clone(),
        sandbox: saved.sandbox.clone(),
        effort: saved.effort.clone(),
        tier: saved.tier.clone(),
        agent_preset: saved.agent_preset.clone(),
    }
}

pub fn saved_settings_from_thread(settings: &ThreadSettings) -> AgentTabSettings {
    AgentTabSettings {
        model: settings.model.clone(),
        approval: settings.approval.clone(),
        approvals_reviewer: settings.approvals_reviewer.clone(),
        sandbox: settings.sandbox.clone(),
        effort: settings.effort.clone(),
        tier: settings.tier.clone(),
        agent_preset: settings.agent_preset.clone(),
    }
}
