//! The controls a conversation runs under -- model, preset, effort -- and the
//! per-profile memory of what the user last picked.
//!
//! Which of these the pane applies and which the harness replays on its own is
//! a capability question, so the picks are remembered here and seeded only
//! where the harness does not restore them itself.

use crate::agent_tab::AgentPane;
use crate::agent_tab::profile::{
    AgentKind, AgentThreadDefaults, agent_launch, defaults_from_thread_settings,
};
use gpui::{App, Context};
use nmt_agent::chat::ThreadSettings;
use nmt_agent::profile::launch_model as effective_launch_model;
use nmt_agent::session::settings::ConversationSettings;
use nmt_config::local_state;
use nmt_config::profile::AgentProfile;
use std::collections::BTreeMap;
use tracing::warn;

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

/// Remember the current thread settings as the defaults for future
/// conversations launched from this profile. Called after every
/// user-driven settings change (dropdowns and slash commands).
pub(crate) fn remember_defaults(
    state: &ConversationSettings,
    kind: AgentKind,
    profile: &AgentProfile,
    cx: &mut Context<AgentPane>,
) {
    let stored = {
        let defaults = cx.default_global::<AgentThreadDefaults>();

        let key = defaults
            .0
            .remember(kind, &profile.name, state.settings.clone());

        let mut stored: BTreeMap<_, _> = defaults_from_thread_settings(defaults);

        stored.retain(|name, _| name == &key);

        stored
    };

    if let Err(err) = local_state::save_agent_defaults(&stored) {
        warn!("failed to save agent defaults to local_state.toml: {err}");
    }
}
