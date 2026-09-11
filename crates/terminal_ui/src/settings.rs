//! The narrow settings surface the terminal reads.
//!
//! The application's settings global carries every configurable value in the
//! program — themes, agent profiles, window chrome — and handing that whole
//! object to the terminal ties the terminal to the settings layer above it.
//! This snapshot holds only what terminal rendering and input actually
//! consume, pre-resolved to plain values. The settings layer rebuilds it
//! whenever the source settings change, so terminal code observes and reads
//! this global alone.

use gpui::{Font, FontFallbacks, Global, Pixels, SharedString, font};
use nmt_config::appearance::InputStyle;
use nmt_config::system::NewlineShortcut;
use nmt_config::{CursorShape, with_active_colors};

use crate::block_list::ITEM_PAD_ROWS;
use crate::block_list::chrome::DurationLabels;
use crate::frame::TerminalColor;
use crate::pane_model::PaneSettings;

pub struct TerminalSettings {
    pub input_style: InputStyle,
    pub cursor_shape: CursorShape,
    /// Wrap spawned shells in a job object so closing the tab tears down the
    /// whole process tree.
    pub manage_subprocess_job: bool,
    /// Draw finished commands as separated blocks with header chrome.
    pub command_blocks: bool,
    pub smooth_wheel: bool,
    pub scroll_to_bottom_when_typing: bool,
    pub newline_shortcut: NewlineShortcut,
    pub font_family: SharedString,
    pub font_size: f32,
    pub line_height: f32,
    /// Tint opacity of the pane background. The window-backdrop and wallpaper
    /// arithmetic that produces it stays with the chrome settings that own
    /// those values; the terminal only paints the result.
    pub background_opacity: f32,
    /// Corner radius shared with the surrounding chrome, so the pane's clip
    /// matches the tab content area it sits in.
    pub corner_radius: Pixels,
    /// Fallback chain appended to the terminal font, matching the CJK
    /// preference the rest of the application text uses.
    pub font_fallbacks: FontFallbacks,
}

impl Global for TerminalSettings {}

impl TerminalSettings {
    pub fn fixed_bottom(&self) -> bool {
        self.input_style.is_fixed_bottom()
    }

    /// The terminal font with the shared fallback chain applied.
    pub fn font(&self) -> Font {
        let mut font = font(self.font_family.clone());
        font.fallbacks = Some(self.font_fallbacks.clone());
        font
    }
}

impl From<&TerminalSettings> for PaneSettings {
    fn from(settings: &TerminalSettings) -> Self {
        Self {
            fixed_bottom: settings.fixed_bottom(),
            pad_rows: if settings.command_blocks {
                ITEM_PAD_ROWS
            } else {
                0.0
            },
            show_block_chrome: settings.command_blocks,
            smooth_wheel: settings.smooth_wheel,
            scroll_to_bottom_when_typing: settings.scroll_to_bottom_when_typing,
            newline_shortcut: settings.newline_shortcut,
            cursor_shape: settings.cursor_shape,
        }
    }
}

pub fn theme_default_background() -> TerminalColor {
    with_active_colors(|colors| colors.background.0.into())
}

pub(crate) fn duration_labels() -> DurationLabels {
    DurationLabels {
        minutes_seconds: nmt_i18n::i18n("terminal-duration-minutes-seconds").to_string(),
        seconds: nmt_i18n::i18n("terminal-duration-seconds").to_string(),
        milliseconds: nmt_i18n::i18n("terminal-duration-milliseconds").to_string(),
    }
}
