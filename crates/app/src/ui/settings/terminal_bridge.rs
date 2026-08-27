//! Keeps the terminal's narrow settings global in step with [`AppSettings`].
//!
//! The terminal reads [`TerminalSettings`] only, so the application-wide
//! settings object stays out of the terminal module. Every write to
//! [`AppSettings`] goes through the global (one `set_global` at startup,
//! `update_global` from the settings pages), so one observer rebuilding the
//! snapshot on each write covers every change path. Panes observe the
//! snapshot global, which puts this rebuild ahead of their reaction by
//! construction.

use gpui::App;
use nmt_app_terminal::settings::TerminalSettings;

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

fn snapshot(cx: &App) -> TerminalSettings {
    let settings = cx.global::<AppSettings>();
    TerminalSettings {
        input_style: settings.input_style,
        cursor_shape: settings.cursor_shape,
        manage_subprocess_job: settings.manage_subprocess_job,
        command_blocks: settings.command_blocks,
        smooth_wheel: settings.smooth_scrolling.terminal_enabled(),
        scroll_to_bottom_when_typing: settings.scroll_to_bottom_when_typing,
        newline_shortcut: settings.newline_shortcut,
        font_family: settings.terminal_font_family.clone(),
        font_size: settings.terminal_font_size as f32,
        line_height: settings.terminal_line_height as f32,
        background_opacity: main_view_background_opacity(cx),
        corner_radius: UI_RADIUS,
        font_fallbacks: default_font_fallbacks(),
    }
}
