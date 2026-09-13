use crate::chat::{SkillCatalog, SlashCommandInfo, ThreadSettings};
use crate::session::Backend;
use crate::session::controller::SessionController;
use crate::session::restore::SettingsSeed;
use crate::session::settings::ConversationSettings;

impl SessionController {
    pub fn controls(&self) -> &ConversationSettings {
        &self.controls
    }

    pub fn command_catalog(&self) -> Option<&[SlashCommandInfo]> {
        self.command_catalog.as_deref()
    }

    pub fn skill_catalog(&self) -> Option<&SkillCatalog> {
        self.skill_catalog.as_ref()
    }

    pub fn reset_for_restart(&mut self) -> Option<Backend> {
        let retiring = self.runtime.retire();

        self.clear_conversation();
        self.controls = ConversationSettings::default();
        self.command_catalog = None;
        self.skill_catalog = None;
        self.commands.clear();

        retiring
    }

    pub fn begin_branched_conversation(&mut self) {
        self.restore.cancel();
        self.controls.seed = SettingsSeed::None;
    }

    pub fn seed_settings(&mut self, seed: SettingsSeed) {
        self.controls.seed = seed;
    }

    pub fn restore_settings_on_ready(&mut self, settings: Option<ThreadSettings>) {
        self.controls.restore_on_ready = settings;
    }

    pub fn set_settings(&mut self, settings: ThreadSettings) {
        self.controls.settings = settings;
    }

    pub fn set_model(&mut self, model: String) {
        if let Some(info) = self.controls.models.iter().find(|info| info.model == model)
            && !self
                .controls
                .settings
                .tier
                .as_ref()
                .is_some_and(|tier| info.tiers.iter().any(|(id, _)| id == tier))
        {
            self.controls.settings.tier = info.default_tier.clone();
        }

        self.controls.settings.model = Some(model);
    }

    pub fn set_approval(&mut self, approval: String) {
        self.controls.settings.approval = Some(approval);
    }

    pub fn set_effort(&mut self, effort: String) {
        self.controls.settings.effort = Some(effort);
    }

    pub fn set_approval_reviewer(&mut self, reviewer: String) {
        self.controls.settings.approvals_reviewer = Some(reviewer);
    }

    pub fn set_sandbox(&mut self, sandbox: String) {
        self.controls.settings.sandbox = Some(sandbox);
    }

    pub fn set_tier(&mut self, tier: Option<String>) {
        self.controls.settings.tier = tier;
    }

    pub fn apply_model_selection(&mut self) -> Option<Result<(), String>> {
        self.controls.apply_model(self.runtime.backend_mut()?)
    }

    pub fn select_agent_preset(&mut self, preset: String) -> Option<Result<(), String>> {
        if self.controls.agent_preset.as_deref() == Some(&preset) {
            return None;
        }

        let result = self.runtime.backend_mut()?.select_agent_preset(&preset);

        if result.is_ok() {
            self.controls.agent_preset = Some(preset);
        }

        Some(result)
    }
}
