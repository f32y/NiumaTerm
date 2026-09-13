//! Backend-neutral rendering data types, relocated from the old `nmt_renderer`
//! crate: a wide-gamut RGBA color (`Color`).

/// RGBA color in linear-light 0..1 space (backend-neutral).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}
