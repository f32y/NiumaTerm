//! The controls a conversation runs under -- model, preset, effort -- and the
//! tab's memory of what the user last picked.
//!
//! Which of these the pane applies and which the harness replays on its own is
//! a capability question, so the picks are remembered here and seeded only
//! where the harness does not restore them itself.

#[cfg(test)]
#[path = "defaults_tests.rs"]
mod defaults_tests;

use gpui::App;
use nmt_agent::profile::launch_model as effective_launch_model;
use nmt_config::profile::AgentProfile;

use crate::agent_tab::AgentPane;
use crate::agent_tab::profile::{AgentKind, agent_launch};

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

/// Remember the conversation's current controls as this tab's own state, so
/// the next conversation it opens starts on them. Called after every
/// user-driven settings change (dropdowns and slash commands). Tabs never
/// share these picks: one opened afterwards starts from its launch profile.
pub(crate) fn remember_defaults(pane: &AgentPane, cx: &mut App) {
    let Some(host) = pane.host.upgrade() else {
        return;
    };

    let settings = pane.session.borrow().controls.settings.clone();

    host.update(cx, |host, _| host.remember_settings(settings));
}
