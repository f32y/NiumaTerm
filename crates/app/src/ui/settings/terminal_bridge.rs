//! Keeps the pane-facing settings globals in step with [`AppSettings`].
//!
//! The terminal reads [`TerminalSettings`] and the agent pane reads
//! [`AgentSettings`], so the application-wide settings object stays out of
//! both. Every write to [`AppSettings`] goes through the global (one
//! `set_global` at startup, `update_global` from the settings pages), so one
//! observer per snapshot rebuilding it on each write covers every change
//! path. Panes observe their snapshot global, which puts the rebuild ahead
//! of their reaction by construction.

use gpui::{App, rgb};
use nmt_agent_ui::settings::AgentSettings;
use nmt_terminal_ui::settings::{TerminalSettings, theme_default_background};

use crate::ui::settings::opacity::main_view_background_opacity;
use crate::ui::settings::state::AppSettings;
use crate::ui::{UI_RADIUS, default_font_fallbacks};

pub(crate) fn install_terminal_settings(cx: &mut App) {
    cx.set_global(snapshot(cx));
    cx.observe_global::<AppSettings>(|cx| {
        let snapshot = snapshot(cx);
        cx.set_global(snapshot);
    })
    .detach();
}

pub(crate) fn install_agent_settings(cx: &mut App) {
    cx.set_global(agent_snapshot(cx));
    cx.observe_global::<AppSettings>(|cx| {
        let snapshot = agent_snapshot(cx);
        cx.set_global(snapshot);
    })
    .detach();
}

fn agent_snapshot(cx: &App) -> AgentSettings {
    let settings = cx.global::<AppSettings>();

    AgentSettings {
        pane_background_follows_terminal: settings.appearance.agent_pane_use_terminal_background,
        font_family: settings.appearance.agent_font_family.clone().into(),
        font_size: settings.appearance.agent_font_size as f32,
        transcript_font_family: settings
            .appearance
            .agent_transcript_font_family
            .clone()
            .into(),
        transcript_font_size: settings.appearance.agent_transcript_font_size as f32,
        newline_shortcut: settings.system.newline_shortcut,
        collapse_tool_calls: settings.agent.collapse_tool_calls,
        codex_skill_command_compat: settings.agent.codex_skill_command_compat,
        model_list_style: settings.agent.model_list_style,
        smooth_wheel: settings.appearance.smooth_scrolling.agent_enabled(),
        reduce_motion: settings.appearance.reduce_motion,
        human_friendly_layout: settings.appearance.human_friendly_agent_ui_layout,
        git_status_refresh_interval: settings.appearance.git_status_refresh_interval,
        profiles: settings.agent_profiles.clone(),
        background_opacity: main_view_background_opacity(cx),
        font_fallbacks: default_font_fallbacks(),
        terminal_background: rgb(theme_default_background().rgb_u32()).into(),
    }
}

fn snapshot(cx: &App) -> TerminalSettings {
    let settings = cx.global::<AppSettings>();

    TerminalSettings {
        input_style: settings.appearance.input_style,
        cursor_shape: settings.cursor_shape,
        manage_subprocess_job: settings.system.manage_subprocess_job,
        command_blocks: settings.appearance.command_blocks,
        smooth_wheel: settings.appearance.smooth_scrolling.terminal_enabled(),
        scroll_to_bottom_when_typing: settings.appearance.scroll_to_bottom_when_typing,
        newline_shortcut: settings.system.newline_shortcut,
        font_family: settings.appearance.terminal_font_family.clone().into(),
        font_size: settings.appearance.terminal_font_size as f32,
        line_height: settings.appearance.terminal_line_height as f32,
        background_opacity: main_view_background_opacity(cx),
        corner_radius: UI_RADIUS,
        font_fallbacks: default_font_fallbacks(),
    }
}
