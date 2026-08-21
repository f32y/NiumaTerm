use gpui::{App, WindowBackgroundAppearance};

use crate::ui::settings::state::{AppSettings, WindowBackdrop};

pub(super) fn effective_background_opacity(backdrop: WindowBackdrop, opacity: f64) -> f64 {
    match backdrop {
        WindowBackdrop::Acrylic => opacity,
        // DWM composes a finished material behind the window, so any tint
        // painted on top only dilutes it and leaves the window looking unlike
        // every other Mica window on the desktop. The material replaces the
        // background outright, which is why the slider is inert in this mode.
        WindowBackdrop::Mica => 0.0,
        WindowBackdrop::Off => 1.0,
    }
}

pub(super) fn effective_surface_background_opacity(
    window_opacity: f64,
    image_opacity: Option<f64>,
) -> f64 {
    window_opacity * (1.0 - image_opacity.unwrap_or(0.0))
}

pub(crate) fn surface_background_opacity(cx: &App) -> f32 {
    let settings = cx.global::<AppSettings>();

    effective_surface_background_opacity(
        effective_background_opacity(settings.window_backdrop, settings.background_opacity),
        settings
            .background_image
            .as_ref()
            .map(|_| settings.background_image_opacity),
    ) as f32
}

/// The tint strength for everything inside a tab (terminal panes, agent pane,
/// right-hand panel). Keeping this at full opacity is what confines the
/// backdrop to the chrome, so tab content stays readable over any wallpaper.
pub(super) fn effective_main_view_background_opacity(
    effect_on_content_area: bool,
    opacity: f32,
) -> f32 {
    if effect_on_content_area { opacity } else { 1.0 }
}

fn effect_on_content_area(cx: &App) -> bool {
    cx.global::<AppSettings>().transparent_main_view
}

pub(crate) fn main_view_background_opacity(cx: &App) -> f32 {
    effective_main_view_background_opacity(
        effect_on_content_area(cx),
        surface_background_opacity(cx),
    )
}

pub(super) fn effective_background_image_layer_opacity(
    window_opacity: f64,
    image_opacity: f64,
) -> f64 {
    let uncovered = 1.0 - effective_surface_background_opacity(window_opacity, Some(image_opacity));

    if uncovered > 0.0 {
        window_opacity * image_opacity / uncovered
    } else {
        0.0
    }
}

pub(crate) fn background_image_layer_opacity(cx: &App) -> f32 {
    let settings = cx.global::<AppSettings>();

    effective_background_image_layer_opacity(
        effective_background_opacity(settings.window_backdrop, settings.background_opacity),
        settings.background_image_opacity,
    ) as f32
}

pub(super) fn window_background_appearance_for(
    backdrop: WindowBackdrop,
) -> WindowBackgroundAppearance {
    match backdrop {
        WindowBackdrop::Acrylic => WindowBackgroundAppearance::Blurred,
        // DWMSBT_TABBEDWINDOW rather than DWMSBT_MAINWINDOW: it carries a
        // stronger wallpaper tint and is what tabbed shells such as Chromium
        // use, so a window next to them reads as the same material. Plain
        // DWMSBT_MAINWINDOW washes out to near-flat gray by comparison.
        WindowBackdrop::Mica => WindowBackgroundAppearance::MicaAltBackdrop,
        WindowBackdrop::Off => WindowBackgroundAppearance::Opaque,
    }
}

/// Select the DWM backdrop material for the configured mode.
pub(crate) fn window_background_appearance(cx: &App) -> WindowBackgroundAppearance {
    window_background_appearance_for(cx.global::<AppSettings>().window_backdrop)
}
