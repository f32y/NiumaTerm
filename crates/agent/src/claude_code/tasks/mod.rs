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

mod records;
mod shells;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::mem::take;
use std::time::SystemTime;

use serde_json::Value;

use crate::background_task::{
    BackgroundTaskDiscoveryState, BackgroundTaskKey, BackgroundTaskKind, BackgroundTaskRefs,
    BackgroundTaskRegistry, BackgroundTaskSnapshot, BackgroundTaskState,
    BackgroundTaskTranscriptUpdate, BackgroundTaskUpdate,
};
use crate::chat::Item;
use crate::claude_code::sessions::RestoredTask;
use crate::claude_code::tasks::records::{
    admits_new_row, lifecycle_state, record_identifiers, refs_from, result_text, sidechain_preview,
    stop_target,
};
use crate::claude_code::tasks::shells::ShellIndex;
use crate::claude_code::tool_items::{complete_tool_item, tool_item};
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
        let Some(registry) = self.registry.as_mut() else {
            return false;
        };

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

                    changed |= registry.merge_restored(key, task.update, starting_sequence);
                }

                changed | registry.set_discovery(BackgroundTaskDiscoveryState::Ready)
            }
            Err(message) => {
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

    fn observe_lifecycle(&mut self, kind: &str, record: &Value) -> bool {
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
    fn observe_subagent_stop(&mut self, record: &Value) -> bool {
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

    /// Observe one incoming message. Returns true when child state changed.
    /// Runs before parent transcript handling, so no parent behavior depends
    /// on what this reducer does or does not recognize.
    pub(crate) fn observe(&mut self, message: &Value) -> bool {
        if let Some(session_id) = message["session_id"].as_str() {
            self.set_session(session_id);
        }

        if self.registry.is_none() {
            return false;
        }

        let linked_parent = message["parent_tool_use_id"].as_str();

        match message["type"].as_str() {
            Some("system") => self.observe_system(message),
            Some("assistant") | Some("stream_event") => match linked_parent {
                Some(parent) => self.observe_sidechain(parent, message),
                None => self.observe_parent_assistant(message),
            },
            Some("user") => match linked_parent {
                Some(parent) => self.observe_sidechain(parent, message),
                None => self.observe_parent_user(message),
            },
            _ => false,
        }
    }

    fn observe_system(&mut self, message: &Value) -> bool {
        let subtype = message["subtype"].as_str().unwrap_or_default();

        match subtype {
            // The CLI emits `init` once per process, so it is the only
            // reliable process boundary in the stream.
            "init" => self.advance_epoch(),
            "hook_started" | "hook_response" => self.observe_hook(message),
            "background_tasks_changed" => self.observe_background_snapshot(message),
            _ if LIFECYCLE_RECORDS.contains(&subtype) => self.observe_lifecycle(subtype, message),
            _ => false,
        }
    }

    /// A new process cannot still be running the children of the previous one.
    fn advance_epoch(&mut self) -> bool {
        self.epoch += 1;

        let epoch = self.epoch;

        let Some(snapshot) = self.snapshot() else {
            return false;
        };

        let stale: Vec<String> = snapshot
            .tasks
            .into_iter()
            .filter(|task| task.state.is_active())
            .map(|task| task.key.id)
            .filter(|id| {
                self.created_epoch
                    .get(id)
                    .is_none_or(|created| *created < epoch)
            })
            .collect();

        let mut changed = false;

        for id in stale {
            changed |= self.apply(
                &id,
                BackgroundTaskUpdate {
                    state: Some(BackgroundTaskState::Stopped),
                    completed_at: Some(SystemTime::now()),
                    updated_at: Some(SystemTime::now()),
                    ..BackgroundTaskUpdate::default()
                },
            );
        }

        changed
    }

    /// Hook events reach the stream only when the CLI was launched with hook
    /// events enabled. A `SubagentStop` identifies its child by `agent_id`
    /// alone, so it lands only when an earlier record tied that id to a task.
    fn observe_hook(&mut self, message: &Value) -> bool {
        let event = message["hook_event"]
            .as_str()
            .or_else(|| message["hook_event_name"].as_str());

        if event != Some("SubagentStop") {
            return false;
        }

        self.observe_subagent_stop(message)
    }

    /// Parent assistant messages carry the `Task`/`Agent` tool-use blocks that
    /// launch a child. The block also stays in the parent transcript as the
    /// tool row the user already sees.
    fn observe_parent_assistant(&mut self, message: &Value) -> bool {
        let mut changed = false;

        for block in message["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if block["type"].as_str() != Some("tool_use") {
                continue;
            }

            let Some(name) = block["name"].as_str() else {
                continue;
            };

            let Some(tool_use_id) = block["id"].as_str() else {
                continue;
            };

            if name == "Bash" {
                // Recorded for every `Bash` call, because whether the command
                // ends up backgrounded is decided after the block is written.
                self.shells
                    .remember_bash_command(tool_use_id, &block["input"]);

                continue;
            }

            if !LAUNCH_TOOLS.contains(&name) {
                continue;
            }

            let input = &block["input"];
            let objective = text_field(input, &["prompt", "task", "instructions"]);

            changed |= self.apply(
                tool_use_id,
                BackgroundTaskUpdate {
                    refs: Some(BackgroundTaskRefs::ClaudeCode {
                        task_id: None,
                        tool_use_id: Some(tool_use_id.to_owned()),
                        agent_id: None,
                    }),
                    // The row exists before any child activity arrives, so a
                    // launched child is visible immediately.
                    state: Some(BackgroundTaskState::Starting),
                    display_name: text_field(input, &["description", "name", "title"]),
                    agent_type: text_field(input, &["subagent_type", "agent_type", "agent"])
                        .or_else(|| Some(name.to_owned())),
                    objective: objective.clone(),
                    model: text_field(input, &["model"]),
                    started_at: Some(SystemTime::now()),
                    updated_at: Some(SystemTime::now()),
                    ..BackgroundTaskUpdate::default()
                },
            );

            changed |= self.open_child_conversation(tool_use_id, objective);
        }

        changed
    }

    /// Open a child's conversation with the instructions it was launched on,
    /// which is what the restored transcript of the same child begins with.
    /// The launch block carries them, so the child reads the same way whether
    /// or not the CLI version streams the child's own copy of the prompt.
    fn open_child_conversation(&mut self, tool_use_id: &str, objective: Option<String>) -> bool {
        let Some(prompt) = objective else {
            return false;
        };

        self.children.open(tool_use_id, prompt)
    }

    /// Parent user messages carry tool results and, in some versions, task
    /// notification records. Ordinary user text and results for other tools
    /// are left untouched.
    fn observe_parent_user(&mut self, message: &Value) -> bool {
        let mut changed = false;

        for block in message["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some("tool_result") = block["type"].as_str() {
                let Some(tool_use_id) = block["tool_use_id"].as_str() else {
                    continue;
                };

                // Only a result matching a known launch is a child
                // outcome; every other tool result belongs to the parent.
                let Some(canonical) = self.canonical(tool_use_id) else {
                    continue;
                };

                // A backgrounded command answers its `Bash` call the moment it
                // is handed off, so its result is the acknowledgement that it
                // started rather than what it did. Its outcome arrives later,
                // as the task records that own the row.
                if self.is_shell(&canonical) {
                    self.shells.remember_handoff_output_file(&canonical, block);

                    continue;
                }

                let failed = block["is_error"].as_bool().unwrap_or(false);

                changed |= self.apply(
                    &canonical,
                    BackgroundTaskUpdate {
                        state: Some(if failed {
                            BackgroundTaskState::Failed
                        } else {
                            BackgroundTaskState::Done
                        }),
                        status: result_text(block),
                        completed_at: Some(SystemTime::now()),
                        updated_at: Some(SystemTime::now()),
                        ..BackgroundTaskUpdate::default()
                    },
                );
            }
        }

        changed
    }

    /// Sidechain activity: the child's own assistant text, reasoning, and tool
    /// calls. It updates the row's latest status and never enters the parent
    /// transcript, which drops these records immediately after this call.
    fn observe_sidechain(&mut self, parent_tool_use_id: &str, message: &Value) -> bool {
        let Some(canonical) = self.canonical(parent_tool_use_id) else {
            // Sidechain traffic with no matching launch belongs to another
            // branch; assigning it to the most recent task would invent a
            // relationship the stream never stated.
            return false;
        };

        let preview = sidechain_preview(message);

        // The same content the parent transcript drops becomes the child's own
        // conversation; it still never reaches the parent.
        let items = self.child_items(&canonical, message);

        self.children.push(&canonical, items);

        // Linked child activity proves the task is doing work, but it cannot
        // revive one that already reported a terminal state.
        let state = self
            .state_of(&canonical)
            .filter(|state| *state == BackgroundTaskState::Starting)
            .map(|_| BackgroundTaskState::Working);

        self.apply(
            &canonical,
            BackgroundTaskUpdate {
                state,
                status: preview.clone(),
                last_preview: preview,
                updated_at: Some(SystemTime::now()),
                ..BackgroundTaskUpdate::default()
            },
        )
    }

    /// Transcript items for one sidechain record, using the same item shapes
    /// the parent conversation renders so a child reads identically.
    fn child_items(&mut self, canonical: &str, message: &Value) -> Vec<Item> {
        let mut items = Vec::new();

        if message["type"].as_str() == Some("user") {
            let text = message["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|block| block["type"].as_str() == Some("text"))
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");

            let repeats_launch = self.children.repeats_launch(canonical, &text);

            if !text.trim().is_empty() && !repeats_launch {
                items.push(Item::UserMessage { text: Some(text) });
            }
        }

        for block in message["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let Some(id) = block["id"]
                .as_str()
                .or_else(|| block["tool_use_id"].as_str())
                .map(str::to_owned)
                .or_else(|| message["uuid"].as_str().map(str::to_owned))
            else {
                continue;
            };

            match block["type"].as_str() {
                Some("text") if message["type"].as_str() != Some("user") => {
                    items.push(Item::AgentMessage {
                        id,
                        text: block["text"].as_str().map(str::to_owned),
                        questions: None,
                    })
                }
                Some("text") => {}
                Some("thinking") => items.push(Item::Reasoning {
                    id,
                    summary: block["thinking"].as_str().map(str::to_owned),
                }),
                Some("tool_use") => {
                    let item = tool_item(
                        &id,
                        block["name"].as_str().unwrap_or("tool"),
                        &block["input"],
                    );

                    self.children.open_tool(id, item.clone());

                    items.push(item);
                }
                Some("tool_result") => {
                    if let Some(started) = self.children.close_tool(&id) {
                        items.push(complete_tool_item(started, block));
                    }
                }
                _ => {}
            }
        }

        items
    }

    /// Admit the background shells the CLI currently reports as running. This
    /// is the only record stating that a command already under way has moved
    /// to the background, which is how a `Bash` call the CLI backgrounds on a
    /// timeout reaches the view at all.
    fn observe_background_snapshot(&mut self, message: &Value) -> bool {
        let mut changed = false;

        for entry in message["tasks"].as_array().into_iter().flatten() {
            if entry["task_type"].as_str() != Some(SHELL_TASK_TYPE) {
                continue;
            }

            // Ambient work is the CLI watching something on its own behalf
            // rather than a command this conversation asked for.
            if entry["ambient"].as_bool().unwrap_or(false) {
                continue;
            }

            let Some(task_id) = entry["task_id"].as_str().filter(|id| !id.is_empty()) else {
                continue;
            };

            // A shell row is only ever created from its task id, by this
            // snapshot or by its own `task_started`, so the row's canonical id
            // and the key its metadata is stored under are the same string.
            let canonical = self
                .canonical(task_id)
                .unwrap_or_else(|| task_id.to_owned());

            self.shells.reserve_shell_meta(task_id);

            // The snapshot lists what is running now. A row that already
            // reported its outcome keeps it: the CLI publishes the snapshot
            // before the terminal record, so re-asserting Working here would
            // resurrect a task that just finished.
            let state = match self.state_of(&canonical) {
                Some(state) if state.is_terminal() => None,
                _ => Some(BackgroundTaskState::Working),
            };

            let meta = self.shells.meta(&canonical);

            let refs = BackgroundTaskRefs::ClaudeCode {
                task_id: Some(task_id.to_owned()),
                tool_use_id: meta.and_then(|meta| meta.tool_use_id.clone()),
                agent_id: None,
            };

            let display_name = text_field(entry, &["description"])
                .or_else(|| meta.and_then(|meta| meta.description.clone()));

            changed |= self.apply(
                &canonical,
                BackgroundTaskUpdate {
                    refs: Some(refs),
                    kind: Some(BackgroundTaskKind::Shell),
                    state,
                    display_name,
                    objective: self.shells.shell_command(&canonical),
                    // The earliest start wins, so re-listing a running command
                    // in a later snapshot leaves its elapsed label alone. No
                    // update time is claimed here: a snapshot is republished
                    // whenever anything in the background set moves, which is
                    // no evidence that this row did anything.
                    started_at: Some(SystemTime::now()),
                    ..BackgroundTaskUpdate::default()
                },
            );
        }

        changed
    }

    fn is_shell(&self, canonical: &str) -> bool {
        self.registry
            .as_ref()
            .and_then(|registry| registry.get(&BackgroundTaskKey::claude_code(canonical)))
            .is_some_and(|task| task.kind == BackgroundTaskKind::Shell)
    }

    /// The command and output file behind one background shell row. Returns
    /// nothing for a row that is not a shell, which is what tells the caller
    /// to read a child conversation instead.
    pub(crate) fn shell_detail(&self, id: &str) -> Option<ShellDetail> {
        let canonical = self.canonical(id)?;

        let task = self
            .registry
            .as_ref()?
            .get(&BackgroundTaskKey::claude_code(&canonical))?;

        if task.kind != BackgroundTaskKind::Shell {
            return None;
        }

        let meta = self.shells.meta(&canonical);

        Some(ShellDetail {
            id: canonical.clone(),
            command: meta.and_then(|meta| meta.command.clone()),
            description: meta.and_then(|meta| meta.description.clone()),
            output_file: meta.and_then(|meta| meta.output_file.clone()),
            state: task.state,
        })
    }
}

// Identifier aliases for one session.
//
// One child is named several ways over its life: by task id, by the tool-use
// id of the call that launched it, and by an agent id. Only a record that
// carried two of them together proves they describe the same child, so this
// records exactly those pairings and nothing inferred from recency.

/// Identifier aliases retained per session. One child contributes at most a
/// handful (task, tool-use, agent), so this only bounds a stream that keeps
/// inventing identifiers.
const MAX_ALIASES: usize = 512;

#[derive(Default)]
struct AliasTable {
    aliases: HashMap<String, String>,
    order: VecDeque<String>,
}

impl AliasTable {
    fn clear(&mut self) {
        self.aliases.clear();

        self.order.clear();
    }

    /// The canonical id this identifier was recorded against, if any.
    fn lookup(&self, id: &str) -> Option<&str> {
        self.aliases.get(id).map(String::as_str)
    }

    /// Record that these identifiers describe the same child. Only called with
    /// identifiers a single record carried together.
    fn link_all(&mut self, canonical: &str, ids: &[String]) {
        for id in ids {
            if id == canonical || self.aliases.contains_key(id) {
                continue;
            }

            if self.order.len() >= MAX_ALIASES
                && let Some(oldest) = self.order.pop_front()
            {
                self.aliases.remove(&oldest);
            }

            self.order.push_back(id.clone());

            self.aliases.insert(id.clone(), canonical.to_owned());
        }
    }
}

// Child conversations accumulated from the sidechain stream.
//
// A child agent has a conversation of its own that the parent transcript
// never shows. The reducer forwards its content rather than retaining it, so
// what lives here is only what a later record needs: the items observed since
// the caller last drained, the tool calls still waiting for their results, and
// the launch instruction already published as the opening message.

#[derive(Default)]
struct ChildTranscripts {
    /// Child conversation content observed since the caller last drained it.
    pending: Vec<(BackgroundTaskKey, BackgroundTaskTranscriptUpdate)>,

    /// Tool calls a child started, so its matching result completes the same
    /// row instead of appearing as a second one.
    open_tools: HashMap<String, Item>,

    /// Launch instructions already published as a child's opening message, by
    /// canonical id. Claude Code 2.1.2x keeps a child's conversation entirely
    /// in its own file and streams only the child's assistant output, so the
    /// launch block is the one place the live stream states what the child was
    /// asked to do. Older versions also replay that text as a sidechain user
    /// record, which `repeats_launch` recognizes as the same instruction rather
    /// than a second one.
    launch_prompts: HashMap<String, String>,
}

impl ChildTranscripts {
    fn clear(&mut self) {
        self.pending.clear();

        self.open_tools.clear();

        self.launch_prompts.clear();
    }

    /// Publish a child's launch instruction as the opening message of its
    /// conversation, reporting whether this is the first time. A second launch
    /// block for the same call states nothing new.
    fn open(&mut self, tool_use_id: &str, prompt: String) -> bool {
        if self.launch_prompts.contains_key(tool_use_id) {
            return false;
        }

        self.launch_prompts
            .insert(tool_use_id.to_owned(), prompt.clone());

        self.pending.push((
            BackgroundTaskKey::claude_code(tool_use_id),
            BackgroundTaskTranscriptUpdate::appended(vec![Item::UserMessage {
                text: Some(prompt),
            }]),
        ));

        true
    }

    /// Add live content to a child's conversation.
    fn push(&mut self, canonical: &str, items: Vec<Item>) {
        if items.is_empty() {
            return;
        }

        self.pending.push((
            BackgroundTaskKey::claude_code(canonical),
            BackgroundTaskTranscriptUpdate::appended(items),
        ));
    }

    /// Offer stored history for a child. History predates whatever the live
    /// stream produced, so it fills a child nothing has been seen for and
    /// never replaces newer live content.
    fn push_restored(&mut self, key: BackgroundTaskKey, items: Vec<Item>) {
        self.pending
            .push((key, BackgroundTaskTranscriptUpdate::restored(items)));
    }

    fn drain(&mut self) -> Vec<(BackgroundTaskKey, BackgroundTaskTranscriptUpdate)> {
        take(&mut self.pending)
    }

    /// Whether this text is the launch instruction already published as the
    /// child's opening message.
    fn repeats_launch(&self, canonical: &str, text: &str) -> bool {
        self.launch_prompts
            .get(canonical)
            .is_some_and(|prompt| prompt.trim() == text.trim())
    }

    fn open_tool(&mut self, id: String, item: Item) {
        self.open_tools.insert(id, item);
    }

    fn close_tool(&mut self, id: &str) -> Option<Item> {
        self.open_tools.remove(id)
    }
}
