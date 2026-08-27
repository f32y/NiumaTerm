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
use nmt_config::CursorShape;
use nmt_config::appearance::InputStyle;
use nmt_config::system::NewlineShortcut;

pub(crate) struct TerminalSettings {
    pub(crate) input_style: InputStyle,
    pub(crate) cursor_shape: CursorShape,
    /// Wrap spawned shells in a job object so closing the tab tears down the
    /// whole process tree.
    pub(crate) manage_subprocess_job: bool,
    /// Draw finished commands as separated blocks with header chrome.
    pub(crate) command_blocks: bool,
    pub(crate) smooth_wheel: bool,
    pub(crate) scroll_to_bottom_when_typing: bool,
    pub(crate) newline_shortcut: NewlineShortcut,
    pub(crate) font_family: SharedString,
    pub(crate) font_size: f32,
    pub(crate) line_height: f32,
    /// Tint opacity of the pane background. The window-backdrop and wallpaper
    /// arithmetic that produces it stays with the chrome settings that own
    /// those values; the terminal only paints the result.
    pub(crate) background_opacity: f32,
    /// Corner radius shared with the surrounding chrome, so the pane's clip
    /// matches the tab content area it sits in.
    pub(crate) corner_radius: Pixels,
    /// Fallback chain appended to the terminal font, matching the CJK
    /// preference the rest of the application text uses.
    pub(crate) font_fallbacks: FontFallbacks,
}

impl Global for TerminalSettings {}

impl TerminalSettings {
    pub(crate) fn fixed_bottom(&self) -> bool {
        self.input_style.is_fixed_bottom()
    }

    /// The terminal font with the shared fallback chain applied.
    pub(crate) fn font(&self) -> Font {
        let mut font = font(self.font_family.clone());
        font.fallbacks = Some(self.font_fallbacks.clone());
        font
    }
}
