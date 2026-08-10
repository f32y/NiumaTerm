use gpui::{App, WindowBackgroundAppearance};

use crate::ui::settings::state::{AppSettings, WindowBackdrop};

pub(super) fn effective_background_opacity(backdrop: WindowBackdrop, opacity: f64) -> f64 {
    match backdrop {
        // Off keeps the window fully opaque; the slider applies in the two
        // translucent modes.
        WindowBackdrop::Acrylic | WindowBackdrop::Mica => opacity,
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

pub(super) fn effective_main_view_background_opacity(transparent: bool, opacity: f32) -> f32 {
    if transparent { opacity } else { 1.0 }
}

fn main_view_is_transparent(cx: &App) -> bool {
    cx.global::<AppSettings>().transparent_main_view
}

pub(crate) fn main_view_background_opacity(cx: &App) -> f32 {
    effective_main_view_background_opacity(
        main_view_is_transparent(cx),
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
        WindowBackdrop::Mica => WindowBackgroundAppearance::MicaBackdrop,
        WindowBackdrop::Off => WindowBackgroundAppearance::Opaque,
    }
}

/// Select the DWM backdrop material for the configured mode.
pub(crate) fn window_background_appearance(cx: &App) -> WindowBackgroundAppearance {
    window_background_appearance_for(cx.global::<AppSettings>().window_backdrop)
}
