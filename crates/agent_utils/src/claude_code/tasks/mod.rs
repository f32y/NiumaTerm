//! Claude Code child-agent reduction for the `Background Tasks` view.
//!
//! Claude describes a child agent across several record shapes: the parent's
//! `Task`/`Agent` tool-use launch, its matching tool result, task lifecycle
//! records, and sidechain traffic tagged with `parent_tool_use_id`. Parent
//! transcript handling deliberately drops the sidechain content so child text
//! is not duplicated under the parent's tool row, so this reducer observes
//! every message first and keeps the child state that would otherwise be lost.
//!
//! Background shells (`Bash` with `run_in_background`, and foreground commands
//! the CLI moves to the background later) are the view's second row kind. They
//! hold no conversation: their lifecycle is the same task records, and their
//! content is a single command plus the file its output is written to.
//!
//! The CLI also publishes a `background_tasks_changed` snapshot of its live
//! background set. It admits shells and nothing else. A shell only becomes
//! backgrounded through a state change that the snapshot reports, whereas a
//! subagent is registered in the foreground and flips later without a second
//! `task_started`, so the same snapshot omits running child agents; agents are
//! therefore still admitted from their own `local_agent` records. Monitors and
//! workflows appear in the snapshot too and stay out by task type.

mod aliases;
mod children;
mod observe;
mod records;
mod shells;

use std::collections::HashMap;
use std::time::SystemTime;

use serde_json::Value;

use crate::background_task::{
    BackgroundTaskDiscoveryState, BackgroundTaskKey, BackgroundTaskKind, BackgroundTaskRegistry,
    BackgroundTaskSnapshot, BackgroundTaskState, BackgroundTaskTranscriptUpdate,
    BackgroundTaskUpdate,
};
use crate::claude_code::sessions::RestoredTask;
use crate::claude_code::tasks::aliases::AliasTable;
use crate::claude_code::tasks::children::ChildTranscripts;
use crate::claude_code::tasks::records::{
    admits_new_row, lifecycle_state, record_identifiers, refs_from, stop_target,
};
use crate::claude_code::tasks::shells::ShellIndex;
use crate::json::text_field;

/// Tool names that launch a child agent.
const LAUNCH_TOOLS: [&str; 2] = ["Task", "Agent"];

/// System subtypes that report one child's lifecycle. Both terminal records
/// matter: a child stopped through the CLI's own stop path reports `killed`
/// only in an update patch, and the matching notification can be suppressed
/// entirely, so watching notifications alone leaves it running forever.
const LIFECYCLE_RECORDS: [&str; 4] = [
    "task_started",
    "task_progress",
    "task_notification",
    "task_updated",
];

/// The task type of delegated agent work. Monitors and workflows travel
/// through the same lifecycle records, so an explicit type is what keeps them
/// out of a view that is about child agents and background shells.
const AGENT_TASK_TYPE: &str = "local_agent";

/// The task type of a shell command the CLI runs as a task. Every `Bash` call
/// registers one; only the backgrounded ones belong in this view, which is why
/// admission tests `is_backgrounded` rather than the type alone.
const SHELL_TASK_TYPE: &str = "local_bash";

#[derive(Default)]
pub(crate) struct ClaudeTasks {
    registry: Option<BackgroundTaskRegistry>,
    /// Task, tool-use, and agent identifiers mapped onto the canonical id of
    /// the row they describe.
    aliases: AliasTable,
    /// Process run each task was first seen in. A task still shown as running
    /// from an earlier run cannot be alive in the current process.
    created_epoch: HashMap<String, u64>,
    /// Advanced by each `init`, which the CLI emits once per process.
    epoch: u64,
    /// The conversations of this session's child agents.
    children: ChildTranscripts,
    /// Background shell metadata and the `Bash` commands behind it.
    shells: ShellIndex,
}

/// What one shell's records have said about it so far. No single record
/// carries all of it: the command comes from the `Bash` block, the description
/// and tool-use id from `task_started`, and the output file from whichever of
/// the handoff result and the completion notification arrives first.
#[derive(Default)]
pub(crate) struct ShellMeta {
    tool_use_id: Option<String>,
    description: Option<String>,
    command: Option<String>,
    output_file: Option<String>,
}

/// One background shell as the detail view reads it. Owned because the caller
/// reads the output file, and holding a borrow of the reducer across that read
/// would pin the whole session state for the duration.
pub(crate) struct ShellDetail {
    /// The row's canonical id, which is also the id of the item the detail
    /// view renders, so repeated reads merge into one card.
    pub(crate) id: String,
    pub(crate) command: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) output_file: Option<String>,
    pub(crate) state: BackgroundTaskState,
}

impl ClaudeTasks {
    pub(crate) fn snapshot(&self) -> Option<BackgroundTaskSnapshot> {
        let mut snapshot = self
            .registry
            .as_ref()
            .map(BackgroundTaskRegistry::snapshot)?;
        // A child is stoppable while it is still running and the stream has
        // named the task the CLI registered it under. A row built only from the
        // parent's tool-use block has no such id yet, and a settled one has
        // nothing left to stop.
        for task in &mut snapshot.tasks {
            task.can_stop = task.state.is_active() && stop_target(&task.refs).is_some();
        }
        Some(snapshot)
    }

    /// The task id to name when stopping one child, resolving the row's key
    /// through the alias table first so a row keyed by its tool-use id still
    /// finds the task id the CLI knows it by.
    pub(crate) fn stop_target(&self, key: &BackgroundTaskKey) -> Option<&str> {
        let registry = self.registry.as_ref()?;
        let canonical = self.aliases.lookup(&key.id);
        let task = registry
            .get(&BackgroundTaskKey::claude_code(
                canonical.unwrap_or(&key.id),
            ))
            .or_else(|| registry.get(key))?;
        task.state.is_active().then_some(())?;
        stop_target(&task.refs)
    }

    pub(crate) fn session_id(&self) -> Option<&str> {
        self.registry
            .as_ref()
            .map(|registry| registry.parent_session().id.as_str())
    }

    /// Mark restoration as running and capture the order counter it started
    /// from. Live updates that land while the read is in flight compare against
    /// this value, so an older file can never replace a newer state.
    pub(crate) fn begin_restoration(&mut self) -> u64 {
        let Some(registry) = self.registry.as_mut() else {
            return 0;
        };
        registry.set_discovery(BackgroundTaskDiscoveryState::Loading);
        registry.sequence()
    }

    /// Fold a completed history read into the registry. A failure keeps every
    /// known live row and is only reported as unavailable when nothing at all
    /// can be shown.
    pub(crate) fn finish_restoration(
        &mut self,
        restored: Result<Vec<RestoredTask>, String>,
        starting_sequence: u64,
    ) -> bool {
        if self.registry.is_none() {
            return false;
        }
        match restored {
            Ok(tasks) => {
                let mut changed = false;
                for task in tasks {
                    let key = BackgroundTaskKey::claude_code(&task.id);
                    if !task.items.is_empty() {
                        // History predates whatever the live stream produced,
                        // so it is offered as a restore: it fills a child
                        // nothing has been seen for and never replaces newer
                        // live content.
                        self.children.push_restored(key.clone(), task.items);
                    }
                    if let Some(registry) = self.registry.as_mut() {
                        changed |= registry.merge_restored(key, task.update, starting_sequence);
                    }
                }
                let registry = self.registry.as_mut().expect("registry exists");
                changed | registry.set_discovery(BackgroundTaskDiscoveryState::Ready)
            }
            Err(message) => {
                let registry = self.registry.as_mut().expect("registry exists");
                if registry.is_empty() {
                    registry.set_discovery(BackgroundTaskDiscoveryState::Unavailable { message })
                } else {
                    registry.set_discovery(BackgroundTaskDiscoveryState::Ready)
                }
            }
        }
    }

    /// Point the reducer at a session. A different id belongs to another
    /// conversation, so its rows and aliases are dropped.
    pub(crate) fn set_session(&mut self, session_id: &str) -> bool {
        if self.session_id() == Some(session_id) {
            return false;
        }
        self.registry = Some(BackgroundTaskRegistry::new(BackgroundTaskKey::claude_code(
            session_id,
        )));
        self.aliases.clear();
        self.created_epoch.clear();
        self.children.clear();
        self.shells.clear();
        true
    }

    /// Take the child conversation content observed since the last call.
    pub(crate) fn take_transcripts(
        &mut self,
    ) -> Vec<(BackgroundTaskKey, BackgroundTaskTranscriptUpdate)> {
        self.children.drain()
    }

    fn apply_lifecycle(&mut self, kind: &str, record: &Value) -> bool {
        // Every task type shares these records. A record that names a type
        // this view does not show is not its work, and one that names no type
        // at all is not assumed to be either kind — it may still enrich a row
        // an earlier record already created.
        let task_type = record["task_type"].as_str();
        if task_type
            .is_some_and(|task_type| task_type != AGENT_TASK_TYPE && task_type != SHELL_TASK_TYPE)
        {
            return false;
        }
        if task_type == Some(SHELL_TASK_TYPE) {
            self.shells.remember_shell(record);
        }
        self.shells.remember_output_file(record);

        let ids = record_identifiers(record);
        let known = ids.iter().any(|id| self.canonical(id).is_some());
        if !known && !admits_new_row(task_type, record) {
            return false;
        }
        let Some(canonical) = self.canonical_from(&ids) else {
            return false;
        };
        self.aliases.link_all(&canonical, &ids);

        let shell = task_type == Some(SHELL_TASK_TYPE) || self.is_shell(&canonical);
        let state = lifecycle_state(kind, record);
        let update = BackgroundTaskUpdate {
            refs: Some(refs_from(record)),
            kind: shell.then_some(BackgroundTaskKind::Shell),
            state,
            display_name: text_field(record, &["description"]),
            // A shell has no agent type to report, and writing its task type
            // into that field would only describe the row as the protocol
            // spells it rather than as anything a reader recognizes.
            agent_type: task_type.filter(|_| !shell).map(str::to_owned),
            // `summary` is the child's own account of what it did; the last
            // tool it ran is the best live substitute while it is working.
            status: text_field(record, &["summary", "last_tool_name"]),
            objective: shell
                .then(|| self.shells.shell_command(&canonical))
                .flatten(),
            completed_at: state
                .filter(|state| state.is_terminal())
                .map(|_| SystemTime::now()),
            updated_at: Some(SystemTime::now()),
            ..BackgroundTaskUpdate::default()
        };
        self.apply(&canonical, update)
    }

    /// A `SubagentStop` hook ends one child. Without a stable identifier that
    /// matches a known task it is ignored, exactly as the parent turn handling
    /// already ignores it, rather than being charged to the newest task.
    fn apply_subagent_stop(&mut self, record: &Value) -> bool {
        let ids = record_identifiers(record);
        let Some(canonical) = self.canonical_from(&ids) else {
            return false;
        };
        // The child already reported its own outcome when a terminal state is
        // set; the hook only closes one that is still shown as running.
        if self
            .state_of(&canonical)
            .is_none_or(BackgroundTaskState::is_terminal)
        {
            return false;
        }
        self.apply(
            &canonical,
            BackgroundTaskUpdate {
                state: Some(BackgroundTaskState::Done),
                completed_at: Some(SystemTime::now()),
                updated_at: Some(SystemTime::now()),
                ..BackgroundTaskUpdate::default()
            },
        )
    }

    fn state_of(&self, canonical: &str) -> Option<BackgroundTaskState> {
        self.registry
            .as_ref()?
            .get(&BackgroundTaskKey::claude_code(canonical))
            .map(|task| task.state)
    }

    /// Canonical id for one identifier: itself when it already names a row.
    fn canonical(&self, id: &str) -> Option<String> {
        if let Some(canonical) = self.aliases.lookup(id) {
            return Some(canonical.to_owned());
        }
        self.registry
            .as_ref()
            .filter(|registry| registry.contains(&BackgroundTaskKey::claude_code(id)))
            .map(|_| id.to_owned())
    }

    /// Canonical id for a record that may carry several identifiers. A record
    /// that matches nothing known still creates a row when it names a task,
    /// because lifecycle records can precede the parent's launch block.
    fn canonical_from(&self, ids: &[String]) -> Option<String> {
        ids.iter()
            .find_map(|id| self.canonical(id))
            .or_else(|| ids.first().cloned())
    }

    fn apply(&mut self, canonical: &str, update: BackgroundTaskUpdate) -> bool {
        let epoch = self.epoch;
        self.created_epoch
            .entry(canonical.to_owned())
            .or_insert(epoch);
        let Some(registry) = self.registry.as_mut() else {
            return false;
        };
        registry.apply(BackgroundTaskKey::claude_code(canonical), update)
    }
}

#[cfg(test)]
mod tests;
