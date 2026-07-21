//! Backend-neutral rendering data types, relocated from the old `nmt_renderer`
//! crate. These are pure config-facing data: a wide-gamut RGBA color
//! (`Color`) and the background-image settings (`ImageProperties`).

use serde::Deserialize;

/// RGBA color in linear-light 0..1 space (backend-neutral).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Color {
    pub const TRANSPARENT: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };
    pub const BLACK: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    pub const WHITE: Self = Self {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };
}

/// Background-image settings read from the config file.
#[derive(Clone, Deserialize, Debug, PartialEq)]
pub struct ImageProperties {
    #[serde(default = "String::default")]
    pub path: String,
    /// Multiplier applied to the image's alpha channel before upload.
    /// Clamped to `[0.0, 1.0]`. `1.0` (the default) means fully opaque.
    #[serde(default = "default_image_opacity")]
    pub opacity: f32,
}

#[inline]
fn default_image_opacity() -> f32 {
    1.0
}

impl Default for ImageProperties {
    fn default() -> Self {
        Self {
            path: String::new(),
            opacity: default_image_opacity(),
        }
    }
}
