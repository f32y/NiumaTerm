//! Claude Code session history: enumerate and replay the transcript files the
//! CLI persists under `~/.claude/projects/<munged-cwd>/<session-id>.jsonl`.
//!
//! The transcript format is an implementation detail of the CLI, so parsing
//! here depends on a minimal field set (`type`, `subtype`, `message.content`,
//! tool block ids/names/inputs, `isSidechain`, `isMeta`, `isCompactSummary`,
//! `compactMetadata`, `uuid`, `gitBranch`) and skips any line it does not
//! recognize — an unparseable session degrades to an id-prefix title instead of
//! failing the list.

mod fork;
mod index;
mod paths;
mod replay;
mod titles;

#[cfg(test)]
use std::collections::HashSet;
#[cfg(test)]
use std::{env, fs};

pub use fork::{ClaudeFork, fork_session_before};
#[cfg(test)]
use fork::{build_fork_records, write_fork_file};
#[cfg(test)]
use index::{TranscriptIndex, is_transcript_entry};
#[cfg(test)]
use paths::munge_cwd;
#[cfg(test)]
use replay::parse_replay;
pub use replay::{load_checkpoints, load_replay};
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use titles::{compaction_summary_text, title_line, user_prompt_text};
pub use titles::{count_sessions, list_sessions};
#[cfg(test)]
use uuid::Uuid;

#[cfg(test)]
use crate::chat::{Compaction, Item};

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

#[cfg(test)]
mod tests;
