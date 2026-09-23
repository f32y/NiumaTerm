pub mod hook;
pub mod sessions;
pub mod stream_json;
pub mod update;
pub mod usage_fetcher;
pub mod workflows;

pub(crate) mod tasks;

mod records;

use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::hook_store::home_dir;

/// The directory Claude Code keeps its settings, sessions and credentials in.
/// The CLI reads `CLAUDE_CONFIG_DIR` and falls back to `~/.claude`, so every
/// file this adapter reads next to the CLI has to resolve the same way.
pub(crate) fn config_home() -> Option<PathBuf> {
    config_home_from(
        env::var_os("CLAUDE_CONFIG_DIR").as_deref(),
        home_dir().as_deref(),
    )
}

/// [`config_home`] for an explicit `CLAUDE_CONFIG_DIR` value and home.
pub(crate) fn config_home_from(config_dir: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    config_dir
        .filter(|path| !path.is_empty())
        .map(Into::into)
        .or_else(|| home.map(|path| path.join(".claude")))
}
