#[cfg(feature = "application")]
pub use crate::application::*;
#[cfg(feature = "application")]
pub use nmt_profile as profile;

pub mod agent;
pub mod appearance;
#[cfg(feature = "application")]
pub mod builtin_themes;
pub mod colors;
pub mod defaults;
#[cfg(feature = "application")]
pub mod local_state;

pub mod remote_session;
pub mod system;
#[cfg(feature = "application")]
pub mod theme;
pub mod update;

#[cfg(feature = "application")]
mod application;
#[cfg(feature = "application")]
mod persistence;

use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::colors::Colors;

/// Cursor shape. Lives here (not in `nmt_terminal::ansi`) because it is a config
/// value `terminal` deserializes; `terminal` re-exports it as `ansi::CursorShape`.
#[derive(Default, Clone, Serialize, Deserialize, Copy, Debug, Eq, PartialEq)]
pub enum CursorShape {
    /// Cursor is a block like `▒`.
    #[default]
    #[serde(alias = "block")]
    Block,

    /// Cursor is an underscore like `_`.
    #[serde(alias = "underline")]
    Underline,

    /// Cursor is a vertical bar `⎸`.
    #[serde(alias = "beam", alias = "line", alias = "Line")]
    Beam,

    /// Cursor is hidden.
    #[serde(alias = "hidden")]
    Hidden,
}

impl From<CursorShape> for char {
    fn from(value: CursorShape) -> Self {
        match value {
            CursorShape::Underline => '_',
            CursorShape::Beam => '|',
            _ => '▇',
        }
    }
}

static ACTIVE_COLORS: OnceLock<RwLock<Colors>> = OnceLock::new();

/// Read from the active terminal palette under its lock. `Colors` carries a
/// field per palette entry, so a caller after one of them reads it here rather
/// than copying several hundred bytes out to reach it.
pub fn with_active_colors<T>(read: impl FnOnce(&Colors) -> T) -> T {
    read(
        &ACTIVE_COLORS
            .get_or_init(|| RwLock::new(Colors::default()))
            .read()
            .expect("active theme colors lock poisoned"),
    )
}

/// Return the active terminal palette. Unlike the rest of the startup config,
/// this value can change when the user selects a theme.
pub fn active_colors() -> Colors {
    with_active_colors(|colors| *colors)
}

pub fn set_active_colors(colors: Colors) {
    *ACTIVE_COLORS
        .get_or_init(|| RwLock::new(colors))
        .write()
        .expect("active theme colors lock poisoned") = colors;
}

impl From<CursorShape> for &'static str {
    fn from(value: CursorShape) -> Self {
        match value {
            CursorShape::Block => "block",
            CursorShape::Underline => "underline",
            CursorShape::Beam => "line",
            CursorShape::Hidden => "hidden",
        }
    }
}

impl From<char> for CursorShape {
    fn from(c: char) -> Self {
        match c {
            '_' => CursorShape::Underline,
            '|' => CursorShape::Beam,
            _ => CursorShape::Block,
        }
    }
}

impl From<&str> for CursorShape {
    fn from(value: &str) -> Self {
        match value {
            "line" => CursorShape::Beam,
            "underline" => CursorShape::Underline,
            _ => CursorShape::Block,
        }
    }
}
