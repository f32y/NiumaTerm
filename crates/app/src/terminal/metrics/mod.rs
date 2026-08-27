use std::slice;

use gpui::{App, TextStyle, Window, px};

use crate::terminal::settings::TerminalSettings;

pub(crate) const COLS: u16 = 100;
pub(crate) const ROWS: u16 = 30;
pub(crate) const PADDING_PX: f32 = 10.0;

pub(crate) fn font_family(cx: &App) -> String {
    cx.global::<TerminalSettings>().font_family.to_string()
}

pub(crate) fn font_size_px(cx: &App) -> f32 {
    cx.global::<TerminalSettings>().font_size
}

pub(crate) fn line_height_multiplier(cx: &App) -> f32 {
    cx.global::<TerminalSettings>().line_height
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CellMetrics {
    pub(crate) width_px: f32,
    pub(crate) height_px: f32,
}

impl CellMetrics {
    /// Grid size for a content rect that already excludes padding (the leaf's
    /// laid-out bounds), so no padding is subtracted here.
    pub(crate) fn grid_size_for_content(self, width_px: f32, height_px: f32) -> (u16, u16) {
        (
            ((width_px / self.width_px).floor() as u16).max(1),
            ((height_px / self.height_px).floor() as u16).max(1),
        )
    }
}

pub(crate) fn terminal_text_style(window: &Window, cx: &App) -> TextStyle {
    let mut style = window.text_style();

    let size = font_size_px(cx);

    style.font_family = font_family(cx).into();
    style.font_fallbacks = Some(cx.global::<TerminalSettings>().font_fallbacks.clone());
    style.font_size = px(size).into();
    style.line_height = px(size * line_height_multiplier(cx)).into();

    style
}

pub(crate) fn measure_cell(window: &mut Window, cx: &App) -> CellMetrics {
    let style = terminal_text_style(window, cx);
    let font_size = style.font_size.to_pixels(window.rem_size());
    let run = style.to_run(1);
    let shaped =
        window
            .text_system()
            .shape_line("0".into(), font_size, slice::from_ref(&run), None);

    CellMetrics {
        width_px: shaped.width().as_f32().max(1.0),
        height_px: style
            .line_height_in_pixels(window.rem_size())
            .as_f32()
            .max(1.0),
    }
}

pub(crate) fn pixel_u16(px: f32) -> u16 {
    px.max(1.0).round().min(u16::MAX as f32) as u16
}

#[cfg(test)]
mod tests;
