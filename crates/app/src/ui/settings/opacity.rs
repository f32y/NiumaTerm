use gpui::{App, WindowBackgroundAppearance};

use super::state::AppSettings;

pub(super) fn effective_background_opacity(transparency_enabled: bool, opacity: f64) -> f64 {
    if transparency_enabled { opacity } else { 1.0 }
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
        effective_background_opacity(
            settings.window_transparency_enabled,
            settings.background_opacity,
        ),
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
        effective_background_opacity(
            settings.window_transparency_enabled,
            settings.background_opacity,
        ),
        settings.background_image_opacity,
    ) as f32
}

pub(super) fn window_background_appearance_for(
    transparency_enabled: bool,
) -> WindowBackgroundAppearance {
    if transparency_enabled {
        WindowBackgroundAppearance::Blurred
    } else {
        WindowBackgroundAppearance::Opaque
    }
}

/// Select acrylic composition only while the alpha-capable target is enabled.
pub(crate) fn window_background_appearance(cx: &App) -> WindowBackgroundAppearance {
    window_background_appearance_for(cx.global::<AppSettings>().window_transparency_enabled)
}
