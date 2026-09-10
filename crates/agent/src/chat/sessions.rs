//! Earlier conversations: how they are listed, what replaying one carries, and
//! where a branch can be cut.
//!
//! A conversation is addressed by an id only its own harness can resolve, so a
//! summary travels with the scope it was listed under.

use std::time::SystemTime;

use crate::chat::Item;

/// Which directories a session listing covers. A conversation is recorded
/// against the directory it ran in, and the tab that lists them is rooted in
/// one, so the two answers a list can give are "this one" and "every one".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionScope {
    #[default]
    CurrentDirectory,
    AllDirectories,
}

/// One resumable persisted session, for the history list an empty chat tab
/// shows above its composer. Ordered newest-first by `last_active`.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSummary {
    pub id: String,
    /// First user prompt of the session (or an id prefix when none exists).
    pub title: String,
    pub branch: Option<String>,
    /// Working directory the session ran in. Carried because a list can span
    /// directories, and resuming a session outside the current one has to
    /// happen where it worked. `None` for a source that does not record it.
    pub cwd: Option<String>,
    pub last_active: SystemTime,
    /// Why a search returned this row. Present only in a list produced by a
    /// content search, because the excerpt describes the query rather than the
    /// session, and an ordinary list has no query to describe.
    pub snippet: Option<String>,
}

/// One turn of a resumed conversation. A live turn's shape comes from the turn
/// lifecycle events — where it started, how long it ran, what it cost, whether
/// the user stopped it — none of which a flat list of items can express, so a
/// replay that dropped it left restored conversations unfoldable and without
/// their durations. Every field is optional because providers persist
/// different parts of it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReplayTurn {
    pub items: Vec<ReplayItem>,
    /// Wall time the turn took.
    pub seconds: Option<u64>,
    /// Output tokens the turn produced.
    pub output_tokens: Option<u64>,
    /// The user stopped the turn before it finished.
    pub interrupted: bool,
}

/// One entry of a resumed turn, with the wall-clock time the provider recorded
/// for it as Unix seconds. Formatting belongs to the UI, which owns the
/// viewer's time zone.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayItem {
    pub item: Item,
    pub at: Option<i64>,
}

/// Where a branch is cut, named the way the backend that owns the conversation
/// names positions in it.
///
/// The three coordinates are not interchangeable: one addresses a transcript
/// record, one a turn, one an event, and only their own backend can resolve
/// them. Carrying the backend's own name for the position keeps this side from
/// maintaining a parallel numbering it would have to hold in step with a
/// history it does not own.
///
/// Every variant names the same cut — the conversation stops before one human
/// prompt — but the backends anchor it from opposite sides, so the variants
/// spell out which side they mean.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForkAnchor {
    /// The Claude transcript record the copied prefix stops before.
    ClaudeBefore(String),
    /// The last Codex turn the copy keeps, inclusive.
    CodexThrough(String),
    /// A DeepSeek event seq lying inside the last turn the copy keeps.
    DeepSeekThrough(u64),
}

/// One human prompt a branch can be cut in front of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkCheckpoint {
    /// The prompt the branch stops in front of, shown as the row's label and
    /// handed back to the composer so the branch starts where it was cut.
    pub prompt: String,
    /// RFC 3339 as the provider recorded it; the UI owns the viewer's zone.
    pub timestamp: Option<String>,
    pub anchor: ForkAnchor,
}
