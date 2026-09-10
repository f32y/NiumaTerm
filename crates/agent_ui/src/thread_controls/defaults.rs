//! The controls a conversation runs under -- model, preset, effort -- and the
//! per-profile memory of what the user last picked.
//!
//! Which of these the pane applies and which the harness replays on its own is
//! a capability question, so the picks are remembered here and seeded only
//! where the harness does not restore them itself.

use gpui::{App, Context};
use nmt_agent::chat::ThreadSettings;
use nmt_agent::profile::launch_model as effective_launch_model;
use nmt_config::local_state;
use nmt_config::profile::AgentProfile;
use tracing::warn;

use crate::AgentPane;
use crate::composer::CommandFeedbackKind;
use crate::profile::{AgentKind, AgentThreadDefaults, agent_launch};
use crate::thread_controls::ThreadControls;

/// The picks remembered for this profile, falling back to the bucket its
/// agent kind shares with unnamed profiles.
pub(crate) fn stored_thread_settings<'a>(
    kind: AgentKind,
    profile: &AgentProfile,
    cx: &'a App,
) -> Option<&'a ThreadSettings> {
    let defaults = cx.try_global::<AgentThreadDefaults>()?;

    defaults.0.get(kind, &profile.name)
}

/// Effective startup model after protocol mapping and user environment
/// overrides. Claude resolves `ANTHROPIC_MODEL` with last-value-wins
/// semantics; Codex receives the profile field over app-server RPC.
pub(crate) fn launch_model(kind: AgentKind, profile: &AgentProfile) -> Option<String> {
    effective_launch_model(kind, agent_launch(profile))
}

/// The reasoning effort this pane's profile pins. Claude receives it as a
/// launch flag and Codex as a thread-start parameter; the picker shows it
/// either way.
pub(crate) fn launch_effort(profile: &AgentProfile) -> Option<String> {
    agent_launch(profile).effort
}

impl ThreadControls {
    /// Remember the current thread settings as the defaults for future
    /// conversations launched from this profile. Called after every
    /// user-driven settings change (dropdowns and slash commands).
    pub(crate) fn remember_defaults(
        &self,
        kind: AgentKind,
        profile: &AgentProfile,
        cx: &mut Context<AgentPane>,
    ) {
        let stored = {
            let defaults = cx.default_global::<AgentThreadDefaults>();

            let key = defaults
                .0
                .remember(kind, &profile.name, self.state.settings.clone());

            let mut stored = defaults.to_local_state();

            stored.retain(|name, _| name == &key);

            stored
        };

        if let Err(err) = local_state::save_agent_defaults(&stored) {
            warn!("failed to save agent defaults to local_state.toml: {err}");
        }
    }
}

impl AgentPane {
    /// Push the current model and effort picks to a harness that applies them
    /// as their own request.
    ///
    /// A refusal restores both pickers from what the session is actually set
    /// to, because a picker left showing a value the harness never adopted
    /// would misreport which model the next turn runs on.
    pub(crate) fn apply_model_selection(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.runtime.backend_mut() else {
            return;
        };
        let Some(outcome) = self.controls.state.apply_model(session) else {
            return;
        };

        match outcome {
            Ok(()) => cx.notify(),
            Err(error) => self
                .palette
                .set_feedback(CommandFeedbackKind::Error, error, cx),
        }
    }

    /// Rebuild this conversation's agent from another composition.
    ///
    /// The harness allows this only before the conversation has run anything,
    /// because the logged history was produced under the previous composition's
    /// tools. That rule is not repeated here: the picker reports whatever the
    /// harness answers, and the row stays on the preset still in force.
    pub(crate) fn apply_agent_preset(&mut self, preset: String, cx: &mut Context<Self>) {
        let Some(session) = self.runtime.backend_mut() else {
            return;
        };

        if self.controls.state.agent_preset.as_deref() == Some(preset.as_str()) {
            return;
        }

        match session.select_agent_preset(&preset) {
            Ok(()) => {
                self.controls.state.agent_preset = Some(preset);
                cx.notify();
            }
            Err(error) => self
                .palette
                .set_feedback(CommandFeedbackKind::Error, error, cx),
        }
    }
}
