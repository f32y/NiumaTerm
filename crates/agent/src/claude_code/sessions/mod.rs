//! Claude Code session history: enumerate and replay the transcript files the
//! CLI persists under `~/.claude/projects/<munged-cwd>/<session-id>.jsonl`.
//!
//! The transcript format is an implementation detail of the CLI, so parsing
//! here depends on a minimal field set (`type`, `subtype`, `message.content`,
//! tool block ids/names/inputs, `isSidechain`, `isMeta`, `isCompactSummary`,
//! `compactMetadata`, `uuid`, `gitBranch`) and skips any line it does not
//! recognize — an unparseable session degrades to an id-prefix title instead of
//! failing the list.

pub use crate::claude_code::sessions::fork::ClaudeFork;

pub(crate) use crate::claude_code::sessions::fork::fork_session_before;
pub(crate) use crate::claude_code::sessions::replay::{load_checkpoints, try_load_replay};
pub(crate) use crate::claude_code::sessions::task_history::{
    RestoredTask, load_child_transcript, load_task_history,
};
pub(crate) use crate::claude_code::sessions::titles::{
    count_all_sessions, count_sessions, list_all_sessions, list_sessions,
    provisional_title_from_prompt,
};

/// The workflow reader resolves the same project directory and parses the same
/// child transcript shape, so both are shared rather than reimplemented.
pub(super) use crate::claude_code::sessions::replay::parse_child_replay;

pub(crate) mod progress;

mod fork;
mod index;
mod replay;
mod task_history;
mod titles;

#[cfg(test)]
mod tests;

#[cfg(test)]
use std::collections::HashSet;
use std::env;
#[cfg(test)]
use std::fs;
use std::path::PathBuf;

#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use uuid::Uuid;

#[cfg(test)]
use crate::chat::Compaction;
#[cfg(test)]
use crate::claude_code::sessions::fork::{build_fork_records, write_fork_file};
#[cfg(test)]
use crate::claude_code::sessions::index::{TranscriptIndex, is_transcript_entry};
#[cfg(test)]
use crate::claude_code::sessions::replay::parse_replay;
#[cfg(test)]
use crate::claude_code::sessions::task_history::{
    load_child_transcript_at, load_task_history_at, parse_task_history,
};
#[cfg(test)]
use crate::claude_code::sessions::titles::{
    compaction_summary_text, recorded_title, resolved_session_title, user_prompt_text,
};
use crate::hook_store::home_dir;

/// Whether the selected user message has a persisted file-history snapshot.
/// `Unknown` is reserved for snapshot records whose schema is not understood;
/// the provider remains the final authority when that happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileRestoreAvailability {
    Available,
    Unavailable,
    Unknown,
}

/// One human prompt that can serve as a Claude rewind target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeCheckpoint {
    pub user_message_id: String,
    pub parent_message_id: Option<String>,
    pub prompt: String,
    pub timestamp: Option<String>,
    pub file_restore_availability: FileRestoreAvailability,
}

// Filesystem path resolution for persisted Claude Code sessions.
//
// Claude chooses one history directory per process working directory. These
// helpers mirror that encoding so listing, replaying, and forking target the
// same JSONL files as the spawned CLI. A missing home directory or working
// directory yields `None`; callers convert that absence into an empty result
// or an operation error appropriate to their API.

/// The CLI resolves `--resume` against the project directory derived from the
/// process cwd, so listing and resuming must use the same directory mapping:
/// every non-ASCII-alphanumeric character becomes `-`.
fn munge_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The directory holding one transcript directory per project.
fn projects_root() -> Option<PathBuf> {
    Some(home_dir()?.join(".claude").join("projects"))
}

/// The transcript directory for `cwd` (falling back to the process cwd, which
/// is what a spawned `claude` without an explicit working directory uses).
pub(super) fn project_dir(cwd: Option<&str>) -> Option<PathBuf> {
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

fn session_path(cwd: Option<&str>, session_id: &str) -> Option<PathBuf> {
    project_dir(cwd).map(|dir| dir.join(format!("{session_id}.jsonl")))
}
