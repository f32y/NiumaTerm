#[cfg(test)]
#[path = "settings_tests.rs"]
mod settings_tests;

use std::mem::take;

use crate::chat::{AgentPreset, ApprovalPreset, ModelInfo, ThreadSettings};
use crate::session::capabilities::AgentCapabilities as _;
use crate::session::restore::SettingsSeed;
use crate::session::{AgentKind, Backend, SettingsOutcome};

#[derive(Default)]
pub struct ConversationSettings {
    /// Current thread settings, seeded from the session's `Ready` event and
    /// changed via the dropdowns under the input; sent as overrides on every
    /// turn start (idempotent when unchanged).
    pub settings: ThreadSettings,

    /// Which local settings the next Ready may overlay. Resumed providers
    /// differ in whether they restore all controls or leave the reviewer local.
    pub seed: SettingsSeed,

    /// A rewind starts a new backend identity but keeps the user's current
    /// thread controls. The first Ready payload describes process defaults,
    /// so these values are overlaid once instead of being replaced by them.
    pub restore_on_ready: Option<ThreadSettings>,

    /// Model catalog; service tiers are per model, so the tier dropdown lists
    /// the selected model's tiers.
    pub models: Vec<ModelInfo>,

    /// Execution-permission presets, for a harness whose preset table belongs
    /// to its deployment. Empty for one whose presets this UI can name
    /// itself.
    pub approval_presets: Vec<ApprovalPreset>,

    /// Agent compositions this deployment offers; the one this conversation
    /// was built from is `settings.agent_preset`. Empty where the deployment
    /// composes none, which is a picker with nothing to choose between rather
    /// than an unsupported one.
    pub agent_presets: Vec<AgentPreset>,
}

/// Overlay remembered controls on what a Ready reported: each remembered
/// pick wins where one exists. A remembered composition already travelled
/// with the creation request, and the harness refuses to recompose a
/// conversation, so the reported one is kept.
fn overlay_remembered(next: ThreadSettings, local: &ThreadSettings) -> ThreadSettings {
    ThreadSettings {
        model: local.model.clone().or(next.model),
        approval: local.approval.clone().or(next.approval),
        approvals_reviewer: local.approvals_reviewer.clone().or(next.approvals_reviewer),
        sandbox: local.sandbox.clone().or(next.sandbox),
        effort: local.effort.clone().or(next.effort),
        tier: local.tier.clone().or(next.tier),
        agent_preset: next.agent_preset,
    }
}

impl ConversationSettings {
    pub fn ready(
        &mut self,
        kind: AgentKind,
        settings: ThreadSettings,
        stored: Option<&ThreadSettings>,
        startup_model: Option<&str>,
        startup_effort: Option<&str>,
    ) {
        let effort = settings.effort.clone().or(self.settings.effort.clone());

        // The composition is reported by its own event rather than with the
        // other controls, so a Ready that carries none keeps the one known.
        let agent_preset = settings
            .agent_preset
            .clone()
            .or(self.settings.agent_preset.clone());

        let mut next = ThreadSettings {
            effort,
            agent_preset,
            ..settings
        };

        // Fresh conversations, and resumes into a harness that does not
        // replay its own controls, seed all remembered picks. Where
        // another Ready arrives during first-turn initialization, that
        // later confirmation preserves the controls in use instead of
        // restoring the ones the CLI reports.
        let (seed_thread_defaults, seed_approval_reviewer) = match take(&mut self.seed) {
            SettingsSeed::Defaults => (true, false),
            SettingsSeed::Reviewer => (false, true),
            SettingsSeed::None => (false, false),
        };

        let preserve_current = kind.caps().repeats_ready_during_init && !seed_thread_defaults;

        let local = if preserve_current {
            Some(&self.settings)
        } else {
            stored
        };

        if (seed_thread_defaults || preserve_current)
            && let Some(local) = local
        {
            next = overlay_remembered(next, local);
        }

        if seed_approval_reviewer
            && let Some(reviewer) = local.and_then(|local| local.approvals_reviewer.clone())
        {
            next.approvals_reviewer = Some(reviewer);
        }

        // A launch profile's pinned model and effort outrank both the thread
        // and the remembered picks, but only when the defaults are seeded.
        if seed_thread_defaults {
            if let Some(model) = startup_model {
                next.model = Some(model.to_string());
            }

            if let Some(effort) = startup_effort {
                next.effort = Some(effort.to_string());
            }
        }

        if let Some(restored) = self.restore_on_ready.take() {
            next = overlay_remembered(next, &restored);
        }

        self.settings = next;
    }

    /// Hand the model and effort picks to the session when they differ from
    /// what it runs under. A request the harness answered, either way, makes
    /// the session's selection the authority, so a refusal puts the pickers
    /// back. A pick still waiting for its answer, or one that rides the next
    /// submission, leaves them as chosen.
    pub(crate) fn apply_model(&mut self, session: &mut Backend) -> Option<SettingsOutcome> {
        let model = self.settings.model.as_deref()?;

        let effort = (session.selection().0 == Some(model))
            .then_some(self.settings.effort.as_deref())
            .flatten();

        if session.selection() == (Some(model), effort) {
            return None;
        }

        let outcome = session.select_model(model, effort);

        match outcome {
            SettingsOutcome::Effective | SettingsOutcome::Refused { .. } => {
                let (model, effort) = session.selection();

                self.settings.model = model.map(str::to_owned);
                self.settings.effort = effort.map(str::to_owned);
            }
            SettingsOutcome::Requested | SettingsOutcome::RidesNextSubmission => {}
        }

        Some(outcome)
    }

    /// Send the approval the picker shows when it differs from the one the
    /// session reported. Only a remembered or restored pick can make them
    /// differ, and a refusal puts the picker back on what the session runs.
    pub(crate) fn apply_approval(
        &mut self,
        session: &mut Backend,
        reported: Option<String>,
    ) -> Option<SettingsOutcome> {
        let approval = self.settings.approval.clone()?;

        if reported.as_deref() == Some(approval.as_str()) {
            return None;
        }

        let outcome = session.select_approval(&approval);

        if matches!(outcome, SettingsOutcome::Refused { .. }) {
            self.settings.approval = reported;
        }

        Some(outcome)
    }

    pub fn seed_settings(&mut self, seed: SettingsSeed) {
        self.seed = seed;
    }

    pub fn restore_settings_on_ready(&mut self, settings: Option<ThreadSettings>) {
        self.restore_on_ready = settings;
    }

    pub fn set_settings(&mut self, settings: ThreadSettings) {
        self.settings = settings;
    }

    pub fn set_model(&mut self, model: String) {
        if let Some(info) = self.models.iter().find(|info| info.model == model)
            && !self
                .settings
                .tier
                .as_ref()
                .is_some_and(|tier| info.tiers.iter().any(|(id, _)| id == tier))
        {
            self.settings.tier = info.default_tier.clone();
        }

        self.settings.model = Some(model);
    }
}
