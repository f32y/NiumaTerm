//! Filesystem path resolution for persisted Claude Code sessions.
//!
//! Claude chooses one history directory per process working directory. These
//! helpers mirror that encoding so listing, replaying, and forking target the
//! same JSONL files as the spawned CLI. A missing home directory or working
//! directory yields `None`; callers convert that absence into an empty result
//! or an operation error appropriate to their API.

use std::env;
use std::path::PathBuf;

use crate::hook_store::home_dir;

/// The CLI resolves `--resume` against the project directory derived from the
/// process cwd, so listing and resuming must use the same directory mapping:
/// every non-ASCII-alphanumeric character becomes `-`.
pub(super) fn munge_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The transcript directory for `cwd` (falling back to the process cwd, which
/// is what a spawned `claude` without an explicit working directory uses).
pub(in crate::claude_code) fn project_dir(cwd: Option<&str>) -> Option<PathBuf> {
    let cwd = match cwd {
        Some(cwd) => cwd.to_string(),
        None => env::current_dir().ok()?.to_string_lossy().into_owned(),
    };

    Some(
        home_dir()?
            .join(".claude")
            .join("projects")
            .join(munge_cwd(&cwd)),
    )
}

pub(super) fn session_path(cwd: Option<&str>, session_id: &str) -> Option<PathBuf> {
    project_dir(cwd).map(|dir| dir.join(format!("{session_id}.jsonl")))
}
