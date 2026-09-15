//! Platform differences of the conversation surfaces this library draws.
//!
//! Views are built once and shared by every platform. The per-component
//! adjustments that differ between hosts come from [`PlatformStyle`], which
//! `MacOs` and `Windows` implement. Both implementations compile on every
//! host, so editing one platform's appearance is type-checked from the other;
//! [`Host`] names the implementation this build targets and is what views
//! read. Views compose a builder hook with `map`.
//!
//! The executable's window chrome has a counterpart of this module. Each
//! crate styles the views it owns, because this library cannot see the
//! executable's chrome types and the chrome's hooks name tab strip types this
//! library has no use for.

// Both implementations compile on every host, so a change to one platform's
// appearance is type-checked while the other one is built. That leaves the
// implementation this build does not target unreferenced.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod macos;
#[cfg_attr(target_os = "macos", allow(dead_code))]
mod windows;

use gpui::{App, Div, Hsla};

#[cfg(target_os = "macos")]
use crate::platform_style::macos::MacOs;
#[cfg(not(target_os = "macos"))]
use crate::platform_style::windows::Windows;

pub(crate) trait PlatformStyle {
    /// The strip listing recent sessions above the composer, drawn over a
    /// pane filled with `background`.
    fn history_strip(strip: Div, background: Hsla, cx: &App) -> Div;
}

/// The conventions this build targets. Linux shares the Windows appearance.
#[cfg(target_os = "macos")]
pub(crate) type Host = MacOs;

#[cfg(not(target_os = "macos"))]
pub(crate) type Host = Windows;
