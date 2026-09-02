//! The controls a conversation runs under -- model, preset, effort -- and the
//! per-profile memory of what the user last picked.
//!
//! Which of these the pane applies and which the harness replays on its own is
//! a capability question, so the picks are remembered here and seeded only
//! where the harness does not restore them itself.

use gpui::{App, Context};
use nmt_agent_utils::chat::ThreadSettings;
use nmt_config::local_state;
use tracing::warn;

use crate::AgentPane;
use crate::composer::CommandFeedbackKind;
use crate::profile::{
    ANTHROPIC_MODEL_ENV, AgentKind, AgentThreadDefaults, agent_launch, launch_env_value,
};

impl AgentPane {
    /// Push the current model and effort picks to a harness that applies them
    /// as their own request.
    ///
    /// A refusal restores both pickers from what the session is actually set
    /// to, because a picker left showing a value the harness never adopted
    /// would misreport which model the next turn runs on.
    pub(crate) fn apply_model_selection(&mut self, cx: &mut Context<Self>) {
        let Some(model) = self.controls.settings.model.clone() else {
            return;
        };
        let Some(session) = self.runtime.backend.as_mut() else {
            return;
        };

        // The levels belong to the exact model route, so an effort picked for
        // the previous model could be one the new route rejects. A model change
        // therefore travels alone and lets the adapter apply its own default,
        // which is also why no caller has to clear the effort itself.
        let effort = (session.selection().0 == Some(model.as_str()))
            .then(|| self.controls.settings.effort.clone())
            .flatten();

        // Seeding the pickers from a remembered pick reaches here too, and it
        // usually names what the session already has.
        if session.selection() == (Some(model.as_str()), effort.as_deref()) {
            return;
        }

        let outcome = session.select_model(&model, effort.as_deref());

        // Whether it was taken or refused, the pickers now show what the
        // session is actually set to: the harness answers a model change with
        // the effort it chose, and a refusal leaves the previous pair standing.
        let (model, effort) = session.selection();
        self.controls.settings.model = model.map(str::to_string);
        self.controls.settings.effort = effort.map(str::to_string);

        match outcome {
            Ok(()) => cx.notify(),
            Err(error) => self.set_command_feedback(CommandFeedbackKind::Error, error, cx),
        }
    }

    /// Rebuild this conversation's agent from another composition.
    ///
    /// The harness allows this only before the conversation has run anything,
    /// because the logged history was produced under the previous composition's
    /// tools. That rule is not repeated here: the picker reports whatever the
    /// harness answers, and the row stays on the preset still in force.
    pub(crate) fn apply_agent_preset(&mut self, preset: String, cx: &mut Context<Self>) {
        let Some(session) = self.runtime.backend.as_mut() else {
            return;
        };
        if self.controls.agent_preset.as_deref() == Some(preset.as_str()) {
            return;
        }

        match session.select_agent_preset(&preset) {
            Ok(()) => {
                self.controls.agent_preset = Some(preset);
                cx.notify();
            }
            Err(error) => self.set_command_feedback(CommandFeedbackKind::Error, error, cx),
        }
    }

    /// Key for the per-profile thread-settings memory; a profile without a
    /// name shares the agent-kind bucket (also the key older local_state
    /// snapshots used).
    pub(super) fn defaults_key(&self) -> String {
        if self.profile.name.trim().is_empty() {
            self.kind.id().to_string()
        } else {
            self.profile.name.clone()
        }
    }

    /// The picks remembered for this profile, falling back to the bucket its
    /// agent kind shares with unnamed profiles.
    pub(super) fn stored_thread_settings<'a>(&self, cx: &'a App) -> Option<&'a ThreadSettings> {
        let defaults = cx.try_global::<AgentThreadDefaults>()?;
        defaults
            .0
            .get(&self.defaults_key())
            .or_else(|| defaults.0.get(self.kind.id()))
    }

    /// Effective startup model after protocol mapping and user environment
    /// overrides. Claude resolves `ANTHROPIC_MODEL` with last-value-wins
    /// semantics; Codex receives the profile field over app-server RPC.
    pub(super) fn profile_model(&self) -> Option<String> {
        let launch = agent_launch(&self.profile);
        match self.kind {
            AgentKind::Claude => launch_env_value(&launch, ANTHROPIC_MODEL_ENV),
            AgentKind::Codex => launch.model,
            // DeepSeek takes a model through a per-session call rather than the
            // launch, and that call is not mapped yet, so the profile field
            // cannot describe what the session will actually run.
            AgentKind::DeepSeek => None,
        }
    }

    /// The reasoning effort this pane's profile pins. Claude receives it as a
    /// launch flag and Codex as a thread-start parameter; the picker shows it
    /// either way.
    pub(super) fn profile_effort(&self) -> Option<String> {
        agent_launch(&self.profile).effort
    }

    /// Remember the pane's current thread settings as the defaults for future
    /// conversations launched from this profile. Called after every
    /// user-driven settings change (dropdowns and slash commands).
    pub(crate) fn remember_thread_defaults(&self, cx: &mut Context<Self>) {
        let stored = {
            let defaults = cx.default_global::<AgentThreadDefaults>();
            defaults
                .0
                .insert(self.defaults_key(), self.controls.settings.clone());
            defaults.to_local_state()
        };

        if let Err(err) = local_state::save_agent_defaults(&stored) {
            warn!("failed to save agent defaults to local_state.toml: {err}");
        }
    }
}
