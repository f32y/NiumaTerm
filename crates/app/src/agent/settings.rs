//! The narrow settings surface the agent pane reads.
//!
//! Mirrors the terminal's snapshot: the application settings global carries
//! every configurable value in the program, and the pane consumes only the
//! handful below, pre-resolved. The settings layer rebuilds this global
//! whenever the source settings change, so pane code observes and reads this
//! type alone.

use gpui::{Font, FontFallbacks, Global, Pixels, SharedString, font, px};
use nmt_config::agent::CollapseRows;
use nmt_config::profile::AgentProfile;
use nmt_config::system::NewlineShortcut;

/// Corner radius shared with the surrounding chrome. Same value as the
/// chrome's own radius constant: pane cards sit inside chrome frames, and a
/// differing radius shows as a sliver of background in every corner.
pub(in crate::agent) const UI_RADIUS: Pixels = px(8.0);

pub(crate) struct AgentSettings {
    /// Paint the pane over the terminal theme's background color instead of
    /// the chrome surface color.
    pub(crate) pane_background_follows_terminal: bool,
    pub(crate) font_family: SharedString,
    pub(crate) font_size: f32,
    pub(crate) transcript_font_family: SharedString,
    pub(crate) transcript_font_size: f32,
    pub(crate) newline_shortcut: NewlineShortcut,
    pub(crate) collapse_tool_calls: CollapseRows,
    /// Offer `$skill` references through the composer even where the harness
    /// resolves them by prompt text.
    pub(crate) codex_skill_command_compat: bool,
    pub(crate) smooth_wheel: bool,
    /// Seconds between branch-label refreshes of the pane's working
    /// directory.
    pub(crate) git_status_refresh_interval: u64,
    /// The configured launch profiles, so a restart picks up edits made since
    /// the pane opened.
    pub(crate) profiles: Vec<AgentProfile>,
    /// Tint opacity of the pane background; the window-backdrop arithmetic
    /// stays with the chrome settings that own it.
    pub(crate) background_opacity: f32,
    /// Fallback chain matching the CJK preference the rest of the
    /// application text uses.
    pub(crate) font_fallbacks: FontFallbacks,
}

impl Global for AgentSettings {}

impl AgentSettings {
    /// `family` with the shared fallback chain applied.
    pub(crate) fn font_with_fallbacks(&self, family: SharedString) -> Font {
        let mut font = font(family);
        font.fallbacks = Some(self.font_fallbacks.clone());
        font
    }

    pub(crate) fn font(&self) -> Font {
        self.font_with_fallbacks(self.font_family.clone())
    }

    pub(crate) fn transcript_font(&self) -> Font {
        self.font_with_fallbacks(self.transcript_font_family.clone())
    }
}

/// Test scaffolding: panes under test read the global like production code,
/// and the defaults mirror a fresh configuration.
impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            pane_background_follows_terminal: false,
            font_family: SharedString::default(),
            font_size: 14.0,
            transcript_font_family: SharedString::default(),
            transcript_font_size: 14.0,
            newline_shortcut: NewlineShortcut::default(),
            collapse_tool_calls: CollapseRows::default(),
            codex_skill_command_compat: false,
            smooth_wheel: true,
            git_status_refresh_interval: 30,
            profiles: Vec::new(),
            background_opacity: 1.0,
            font_fallbacks: FontFallbacks::default(),
        }
    }
}
