//! Application presentation and embedded translations shared with examples.

pub mod agent_tab;
pub mod assets;
pub mod syntax;
pub mod terminal_tab;
pub mod utils;

#[cfg(test)]
mod localization_tests;

rust_i18n::i18n!("locales", fallback = "en");

// The translation macro reads outside Rust's dependency tracking. These inputs
// let compiler caches invalidate this target when either catalog changes.
const _: (&str, &str) = (
    include_str!("../locales/en.toml"),
    include_str!("../locales/zh-CN.toml"),
);
