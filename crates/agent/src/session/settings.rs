use std::collections::HashMap;
use std::mem::take;

use crate::chat::{AgentPreset, ApprovalPreset, ModelInfo, ThreadSettings};
use crate::session::capabilities::AgentCapabilities as _;
use crate::session::{AgentKind, Backend};

#[derive(Default)]
pub struct ConversationSettings {
    /// Current thread settings, seeded from the session's `Ready` event and
    /// changed via the dropdowns under the input; sent as overrides on every
    /// turn start (idempotent when unchanged).
    pub settings: ThreadSettings,

    /// Whether the next `Ready` should overlay all remembered settings. True
    /// for fresh conversations and resumed Claude conversations; later Claude
    /// confirmations keep the values currently selected under the input.
    pub seed_thread_defaults: bool,

    /// Whether the next resumed Codex thread should take the locally
    /// remembered approval reviewer while preserving its other stored
    /// settings.
    pub seed_approval_reviewer: bool,

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

    /// Agent compositions this deployment offers, and the one this
    /// conversation was built from. Empty where the deployment composes none,
    /// which is a picker with nothing to choose between rather than an
    /// unsupported one.
    pub agent_presets: Vec<AgentPreset>,

    pub agent_preset: Option<String>,
}

/// Fold the thread's reported settings together with what the pane
/// remembered. `startup_model` and `startup_effort` come from the launch
/// profile and are applied last, so a profile that pins one of them wins over
/// both the remembered pick and whatever the agent reported.
pub fn resolve_ready_settings(
    mut next: ThreadSettings,
    local: Option<&ThreadSettings>,
    use_all_local: bool,
    use_local_reviewer: bool,
    startup_model: Option<&str>,
    startup_effort: Option<&str>,
) -> ThreadSettings {
    if use_all_local && let Some(local) = local {
        next = ThreadSettings {
            model: local.model.clone().or(next.model),
            approval: local.approval.clone().or(next.approval),
            approvals_reviewer: local.approvals_reviewer.clone().or(next.approvals_reviewer),
            sandbox: local.sandbox.clone().or(next.sandbox),
            effort: local.effort.clone().or(next.effort),
            tier: local.tier.clone().or(next.tier),
        };
    }

    if use_local_reviewer
        && let Some(reviewer) = local.and_then(|local| local.approvals_reviewer.clone())
    {
        next.approvals_reviewer = Some(reviewer);
    }

    if let Some(model) = startup_model {
        next.model = Some(model.to_string());
    }

    if let Some(effort) = startup_effort {
        next.effort = Some(effort.to_string());
    }

    next
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

        let mut next = ThreadSettings { effort, ..settings };

        // Fresh conversations, and resumes into a harness that does not
        // replay its own controls, seed all remembered picks. Where
        // another Ready arrives during first-turn initialization, that
        // later confirmation preserves the controls in use instead of
        // restoring the ones the CLI reports.
        let seed_thread_defaults = take(&mut self.seed_thread_defaults);
        let seed_approval_reviewer = take(&mut self.seed_approval_reviewer);
        let preserve_current = kind.caps().repeats_ready_during_init && !seed_thread_defaults;

        let local = if preserve_current {
            Some(&self.settings)
        } else {
            stored
        };

        let startup_model = seed_thread_defaults.then_some(startup_model).flatten();
        let startup_effort = seed_thread_defaults.then_some(startup_effort).flatten();

        next = resolve_ready_settings(
            next,
            local,
            seed_thread_defaults || preserve_current,
            seed_approval_reviewer,
            startup_model,
            startup_effort,
        );

        if let Some(restored) = self.restore_on_ready.take() {
            next = resolve_ready_settings(next, Some(&restored), true, false, None, None);
        }

        self.settings = next;
    }

    pub fn apply_model(&mut self, session: &mut Backend) -> Option<Result<(), String>> {
        let model = self.settings.model.as_deref()?;

        let effort = (session.selection().0 == Some(model))
            .then_some(self.settings.effort.as_deref())
            .flatten();

        if session.selection() == (Some(model), effort) {
            return None;
        }

        let outcome = session.select_model(model, effort);
        let (model, effort) = session.selection();

        self.settings.model = model.map(str::to_owned);
        self.settings.effort = effort.map(str::to_owned);

        Some(outcome)
    }
}

#[derive(Default)]
pub struct RememberedSettings(HashMap<String, ThreadSettings>);

impl RememberedSettings {
    pub fn from_entries(entries: impl IntoIterator<Item = (String, ThreadSettings)>) -> Self {
        Self(entries.into_iter().collect())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &ThreadSettings)> {
        self.0.iter()
    }

    pub fn get(&self, kind: AgentKind, profile_name: &str) -> Option<&ThreadSettings> {
        self.0
            .get(Self::key(kind, profile_name))
            .or_else(|| self.0.get(kind.into()))
    }

    pub fn remember(
        &mut self,
        kind: AgentKind,
        profile_name: &str,
        settings: ThreadSettings,
    ) -> String {
        let key = Self::key(kind, profile_name).to_owned();

        self.0.insert(key.clone(), settings);

        key
    }

    fn key(kind: AgentKind, profile_name: &str) -> &str {
        if profile_name.trim().is_empty() {
            kind.into()
        } else {
            profile_name
        }
    }
}
