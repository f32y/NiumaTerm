//! Application presentation and embedded translations shared with examples.

pub use crate::i18n::_rust_i18n_try_translate;

pub(crate) use crate::i18n::_rust_i18n_t;

pub mod agent_tab;
pub mod assets;
pub mod design;
pub mod platform_style;
pub mod syntax;
pub mod terminal_tab;
pub mod utils;

mod i18n;

#[cfg(test)]
mod localization_tests;
