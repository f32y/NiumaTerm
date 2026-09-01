//! Runtime string translation.
//!
//! Both language catalogs are embedded at compile time and parsed once into
//! immortal maps, so [`i18n`] can hand out borrowed `&str` values that stay
//! valid across live language switches: switching only flips an atomic index
//! into the already-loaded catalogs and never drops or replaces a map.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

#[cfg(test)]
mod tests;

pub(crate) const EN: &str = include_str!("../locales/en.toml");
pub(crate) const ZH_CN: &str = include_str!("../locales/zh-CN.toml");

static CATALOGS: OnceLock<[HashMap<String, String>; 2]> = OnceLock::new();
static ACTIVE: AtomicU8 = AtomicU8::new(0);

pub(crate) fn parse_catalog(source: &str) -> HashMap<String, String> {
    // The catalogs are compile-time assets restricted to flat string values;
    // a malformed file is a build defect caught by the crate's unit tests, so
    // failing loudly here beats limping along with missing translations.
    toml::from_str(source).expect("embedded locale file must be flat string-valued TOML")
}

fn language_index(locale: &str) -> u8 {
    match locale {
        "zh-CN" => 1,
        // Unknown or missing config values fall back to English so a stale
        // or hand-edited config never breaks startup.
        _ => 0,
    }
}

/// Parses the embedded catalogs and selects the startup language. Call once
/// before any UI is built; later language changes go through [`set_language`].
pub fn init(locale: &str) {
    CATALOGS.get_or_init(|| [parse_catalog(EN), parse_catalog(ZH_CN)]);
    set_language(locale);
}

/// Switches the active language for all subsequent [`i18n`] lookups.
pub fn set_language(locale: &str) {
    ACTIVE.store(language_index(locale), Ordering::Relaxed);
}

/// Which catalog [`i18n`] is currently answering from. A caller that memoizes
/// translated text keys its cache on this so a live language switch cannot
/// leave the previous language's strings on screen.
pub fn active_language() -> u8 {
    ACTIVE.load(Ordering::Relaxed)
}

/// Returns the active-language text for `key`, or `key` itself when it has no
/// catalog entry, so a typo'd key shows up on screen instead of crashing.
///
/// The result borrows for the whole program: both catalogs are parsed once
/// into maps that are never dropped or replaced, and a `'static` key covers the
/// miss case. Callers can therefore hold the text, or hand it to a type that
/// wraps a `&'static str`, without copying it onto the heap.
pub fn i18n(key: &'static str) -> &'static str {
    // Helper binaries and unit tests can render labels without running the app
    // startup path. Loading the immutable catalogs here preserves the English
    // default while the main app still selects its configured language first.
    let catalogs = CATALOGS.get_or_init(|| [parse_catalog(EN), parse_catalog(ZH_CN)]);
    let catalog = &catalogs[ACTIVE.load(Ordering::Relaxed) as usize];
    match catalog.get(key) {
        Some(value) => value.as_str(),
        None => {
            #[cfg(debug_assertions)]
            tracing::warn!(key, "missing i18n key");
            key
        }
    }
}
