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
use crate::claude_code::tool_items::{complete_tool_item, tool_item};
use crate::json::{condense, text_field};

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

/// Shell entries retained per session, applied to both the metadata table and
/// the command table beside it. Each entry is a few short strings, so this only
/// caps a stream that keeps registering commands.
const MAX_SHELL_META: usize = 256;

/// Identifier aliases retained per session. One child contributes at most a
/// handful (task, tool-use, agent), so this only bounds a stream that keeps
/// inventing identifiers.
const MAX_ALIASES: usize = 512;

#[derive(Default)]
pub(crate) struct ClaudeTasks {
    registry: Option<BackgroundTaskRegistry>,
    /// Task, tool-use, and agent identifiers mapped onto the canonical id of
    /// the row they describe. Only records that carry two identifiers together
    /// create an alias; recency is never used to guess a relationship.
    aliases: HashMap<String, String>,
    alias_order: VecDeque<String>,
    /// Process run each task was first seen in. A task still shown as running
    /// from an earlier run cannot be alive in the current process.
    created_epoch: HashMap<String, u64>,
    /// Advanced by each `init`, which the CLI emits once per process.
    epoch: u64,
    /// Child conversation content observed since the caller last drained it.
    /// The reducer forwards items rather than retaining them, so a child's
    /// conversation is stored once, where it is read.
    pending_transcripts: Vec<(BackgroundTaskKey, BackgroundTaskTranscriptUpdate)>,
    /// Tool calls a child started, so its matching result completes the same
    /// row instead of appearing as a second one.
    open_child_tools: HashMap<String, Item>,
    /// Launch instructions already published as a child's opening message, by
    /// canonical id. Claude Code 2.1.2x keeps a child's conversation entirely
    /// in its own file and streams only the child's assistant output, so the
    /// launch block is the one place the live stream states what the child was
    /// asked to do. Older versions also replay that text as a sidechain user
    /// record, which this recognizes as the same instruction rather than a
    /// second one.
    launch_prompts: HashMap<String, String>,
    /// Everything a shell row needs that its lifecycle records do not all
    /// carry at once, keyed by task id. Recorded for every registered shell,
    /// including foreground ones, because a command the CLI moves to the
    /// background later announces only its task id when it does.
    shell_meta: HashMap<String, ShellMeta>,
    shell_meta_order: VecDeque<String>,
    /// Command text of recent `Bash` tool calls, keyed by tool-use id. The
    /// task records name the shell's description but never its command, so the
    /// launching block is where a row's command comes from.
    bash_commands: HashMap<String, String>,
    bash_command_order: VecDeque<String>,
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
        let canonical = self.aliases.get(key.id.as_str()).map(String::as_str);
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
                        self.pending_transcripts.push((
                            key.clone(),
                            BackgroundTaskTranscriptUpdate::restored(task.items),
                        ));
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
        self.alias_order.clear();
        self.created_epoch.clear();
        self.pending_transcripts.clear();
        self.open_child_tools.clear();
        self.launch_prompts.clear();
        self.shell_meta.clear();
        self.shell_meta_order.clear();
        self.bash_commands.clear();
        self.bash_command_order.clear();
        true
    }

    /// Take the child conversation content observed since the last call.
    pub(crate) fn take_transcripts(
        &mut self,
    ) -> Vec<(BackgroundTaskKey, BackgroundTaskTranscriptUpdate)> {
        take(&mut self.pending_transcripts)
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
            _ if LIFECYCLE_RECORDS.contains(&subtype) => self.apply_lifecycle(subtype, message),
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
        self.apply_subagent_stop(message)
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
                self.remember_bash_command(tool_use_id, &block["input"]);
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
        if self.launch_prompts.contains_key(tool_use_id) {
            return false;
        }
        self.launch_prompts
            .insert(tool_use_id.to_owned(), prompt.clone());
        self.pending_transcripts.push((
            BackgroundTaskKey::claude_code(tool_use_id),
            BackgroundTaskTranscriptUpdate::appended(vec![Item::UserMessage {
                text: Some(prompt),
            }]),
        ));
        true
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
                    self.remember_handoff_output_file(&canonical, block);
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
        if !items.is_empty() {
            self.pending_transcripts.push((
                BackgroundTaskKey::claude_code(&canonical),
                BackgroundTaskTranscriptUpdate::appended(items),
            ));
        }
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
            let repeats_launch = self
                .launch_prompts
                .get(canonical)
                .is_some_and(|prompt| prompt.trim() == text.trim());
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
                    self.open_child_tools.insert(id, item.clone());
                    items.push(item);
                }
                Some("tool_result") => {
                    if let Some(started) = self.open_child_tools.remove(&id) {
                        items.push(complete_tool_item(started, block));
                    }
                }
                _ => {}
            }
        }
        items
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
            self.remember_shell(record);
        }
        self.remember_output_file(record);

        let ids = record_identifiers(record);
        let known = ids.iter().any(|id| self.canonical(id).is_some());
        if !known && !admits_new_row(task_type, record) {
            return false;
        }
        let Some(canonical) = self.canonical_from(&ids) else {
            return false;
        };
        self.link_all(&canonical, &ids);

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
            objective: shell.then(|| self.shell_command(&canonical)).flatten(),
            completed_at: state
                .filter(|state| state.is_terminal())
                .map(|_| SystemTime::now()),
            updated_at: Some(SystemTime::now()),
            ..BackgroundTaskUpdate::default()
        };
        self.apply(&canonical, update)
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
            self.reserve_shell_meta(task_id);
            // The snapshot lists what is running now. A row that already
            // reported its outcome keeps it: the CLI publishes the snapshot
            // before the terminal record, so re-asserting Working here would
            // resurrect a task that just finished.
            let state = match self.state_of(&canonical) {
                Some(state) if state.is_terminal() => None,
                _ => Some(BackgroundTaskState::Working),
            };
            let meta = self.shell_meta.get(&canonical);
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
                    objective: self.shell_command(&canonical),
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

    /// Keep what a shell record carries whether or not the shell is
    /// backgrounded yet. `task_started` is the only record naming both the
    /// task and the `Bash` call behind it, so it is the one chance to tie a
    /// row to the command it runs.
    fn remember_shell(&mut self, record: &Value) {
        let Some(task_id) = record["task_id"].as_str().filter(|id| !id.is_empty()) else {
            return;
        };
        self.reserve_shell_meta(task_id);
        let tool_use_id = text_field(record, &["tool_use_id"]);
        let command = tool_use_id
            .as_deref()
            .and_then(|id| self.bash_commands.get(id))
            .cloned();
        let description = text_field(record, &["description"]);
        let meta = self.shell_meta.entry(task_id.to_owned()).or_default();
        if tool_use_id.is_some() {
            meta.tool_use_id = tool_use_id;
        }
        if description.is_some() {
            meta.description = description;
        }
        if command.is_some() {
            meta.command = command;
        }
    }

    /// The handoff result names the output file while the command is still
    /// running, which is the only point before completion where the path is
    /// stated. Without it a running command has nothing to show until it ends.
    fn remember_handoff_output_file(&mut self, canonical: &str, block: &Value) {
        let Some(meta) = self.shell_meta.get(canonical) else {
            return;
        };
        if meta.output_file.is_some() {
            return;
        }
        let Some(text) = result_content(block) else {
            return;
        };
        let Some(path) = handoff_output_file(&text, canonical) else {
            return;
        };
        if let Some(meta) = self.shell_meta.get_mut(canonical) {
            meta.output_file = Some(path);
        }
    }

    /// The completion notification is where a shell states the file its output
    /// was written to. It omits the task type, so the path is stored against
    /// whichever shell the task id already named.
    fn remember_output_file(&mut self, record: &Value) {
        let Some(output_file) = text_field(record, &["output_file"]) else {
            return;
        };
        let Some(task_id) = record["task_id"].as_str().filter(|id| !id.is_empty()) else {
            return;
        };
        if let Some(meta) = self.shell_meta.get_mut(task_id) {
            meta.output_file = Some(output_file);
        }
    }

    fn remember_bash_command(&mut self, tool_use_id: &str, input: &Value) {
        let Some(command) = text_field(input, &["command"]) else {
            return;
        };
        if !self.bash_commands.contains_key(tool_use_id) {
            if self.bash_command_order.len() >= MAX_SHELL_META
                && let Some(oldest) = self.bash_command_order.pop_front()
            {
                self.bash_commands.remove(&oldest);
            }
            self.bash_command_order.push_back(tool_use_id.to_owned());
        }
        self.bash_commands.insert(tool_use_id.to_owned(), command);
    }

    fn reserve_shell_meta(&mut self, task_id: &str) {
        if self.shell_meta.contains_key(task_id) {
            return;
        }
        if self.shell_meta_order.len() >= MAX_SHELL_META
            && let Some(oldest) = self.shell_meta_order.pop_front()
        {
            self.shell_meta.remove(&oldest);
        }
        self.shell_meta_order.push_back(task_id.to_owned());
    }

    fn shell_command(&self, canonical: &str) -> Option<String> {
        self.shell_meta.get(canonical)?.command.clone()
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
        let meta = self.shell_meta.get(&canonical);
        Some(ShellDetail {
            id: canonical.clone(),
            command: meta.and_then(|meta| meta.command.clone()),
            description: meta.and_then(|meta| meta.description.clone()),
            output_file: meta.and_then(|meta| meta.output_file.clone()),
            state: task.state,
        })
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
        if let Some(canonical) = self.aliases.get(id) {
            return Some(canonical.clone());
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

    /// Record that these identifiers describe the same child. Only called with
    /// identifiers a single record carried together.
    fn link_all(&mut self, canonical: &str, ids: &[String]) {
        for id in ids {
            if id == canonical || self.aliases.contains_key(id) {
                continue;
            }
            if self.alias_order.len() >= MAX_ALIASES
                && let Some(oldest) = self.alias_order.pop_front()
            {
                self.aliases.remove(&oldest);
            }
            self.alias_order.push_back(id.clone());
            self.aliases.insert(id.clone(), canonical.to_owned());
        }
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

/// The output file named in a backgrounded command's handoff result. The file
/// is `<task id>.output`, so that name locates it inside the sentence; the path
/// itself starts at the last `": "` before it, which keeps a directory
/// containing spaces intact where splitting on whitespace would truncate it.
fn handoff_output_file(text: &str, task_id: &str) -> Option<String> {
    let name = format!("{task_id}.output");
    let end = text.find(&name)? + name.len();
    let start = text[..end].rfind(": ")? + 2;
    let path = text[start..end].trim();

    (!path.is_empty()).then(|| path.to_owned())
}

/// Whether a lifecycle record may create a row nothing is known about yet. A
/// child agent is admitted from its own type alone. A shell is admitted only
/// once it reports being backgrounded, because every `Bash` call registers one
/// and the foreground ones are already visible as the tool row that started
/// them.
fn admits_new_row(task_type: Option<&str>, record: &Value) -> bool {
    match task_type {
        Some(AGENT_TASK_TYPE) => true,
        Some(SHELL_TASK_TYPE) => record["is_backgrounded"].as_bool().unwrap_or(false),
        _ => false,
    }
}

/// Every identifier a lifecycle record supplies. Their presence together in one
/// record is what makes them aliases of the same child.
fn record_identifiers(record: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    for key in ["task_id", "tool_use_id", "agent_id"] {
        if let Some(id) = record[key].as_str().filter(|id| !id.is_empty())
            && !ids.iter().any(|known| known == id)
        {
            ids.push(id.to_owned());
        }
    }
    ids
}

/// The identifier `stop_task` accepts. The CLI registers a delegated agent
/// under its task id and an agent task's id is that same value, so either names
/// the child; a tool-use id belongs to the parent's tool call instead and would
/// not be found in the task registry.
fn stop_target(refs: &BackgroundTaskRefs) -> Option<&str> {
    match refs {
        BackgroundTaskRefs::ClaudeCode {
            task_id, agent_id, ..
        } => task_id.as_deref().or(agent_id.as_deref()),
        BackgroundTaskRefs::Codex { .. } | BackgroundTaskRefs::DeepSeek { .. } => None,
    }
}

fn refs_from(record: &Value) -> BackgroundTaskRefs {
    BackgroundTaskRefs::ClaudeCode {
        task_id: text_field(record, &["task_id"]),
        tool_use_id: text_field(record, &["tool_use_id"]),
        agent_id: text_field(record, &["agent_id"]),
    }
}

/// Lifecycle state a task record reports. `task_notification` carries only
/// terminal statuses; `task_updated` carries the full vocabulary and is the
/// only place a stopped task's `killed` reliably appears.
fn lifecycle_state(kind: &str, record: &Value) -> Option<BackgroundTaskState> {
    let status = match kind {
        "task_started" => return Some(BackgroundTaskState::Working),
        "task_progress" => return Some(BackgroundTaskState::Working),
        "task_notification" => record["status"].as_str()?,
        "task_updated" => record["patch"]["status"]
            .as_str()
            .or_else(|| record["status"].as_str())?,
        _ => return None,
    };
    Some(match status {
        "pending" => BackgroundTaskState::Starting,
        "running" => BackgroundTaskState::Working,
        // A paused background task is waiting on something outside itself,
        // which is what the panel shows as needing input.
        "paused" => BackgroundTaskState::NeedsInput,
        "completed" => BackgroundTaskState::Done,
        "failed" => BackgroundTaskState::Failed,
        "stopped" | "killed" => BackgroundTaskState::Stopped,
        _ => return None,
    })
}

fn result_text(block: &Value) -> Option<String> {
    condense(&result_content(block)?)
}

/// A tool result's text as the CLI wrote it. Kept apart from the condensed
/// preview a row shows, because a handoff result states a path that is longer
/// than the preview bound and would be cut in half by it.
fn result_content(block: &Value) -> Option<String> {
    let content = &block["content"];
    content.as_str().map(str::to_owned).or_else(|| {
        let parts: Vec<&str> = content
            .as_array()?
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect();
        (!parts.is_empty()).then(|| parts.join("\n"))
    })
}

/// One-line summary of a child's latest sidechain record.
fn sidechain_preview(message: &Value) -> Option<String> {
    for block in message["message"]["content"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let text = match block["type"].as_str() {
            Some("text") => block["text"].as_str(),
            Some("thinking") => block["thinking"].as_str(),
            Some("tool_use") => block["name"].as_str(),
            _ => None,
        };
        if let Some(condensed) = text.and_then(condense) {
            return Some(condensed);
        }
    }
    None
}

#[cfg(test)]
mod tests;
