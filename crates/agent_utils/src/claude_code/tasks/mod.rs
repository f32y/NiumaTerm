//! Claude Code child-agent reduction for the `Background Tasks` view.
//!
//! Claude describes a child agent across several record shapes: the parent's
//! `Task`/`Agent` tool-use launch, its matching tool result, task lifecycle
//! records, and sidechain traffic tagged with `parent_tool_use_id`. Parent
//! transcript handling deliberately drops the sidechain content so child text
//! is not duplicated under the parent's tool row, so this reducer observes
//! every message first and keeps the child state that would otherwise be lost.
//!
//! The CLI also publishes a `background_tasks_changed` snapshot of its live
//! background set. It is deliberately not consumed: a subagent is registered in
//! the foreground and only flips to backgrounded later without a second
//! `task_started`, so the snapshot omits running child agents, while spanning
//! shells, monitors, and workflows this view must exclude. Narrowing against it
//! would retire agents that are still working, and widening from it would admit
//! work that is not a child agent at all.

use std::collections::{HashMap, VecDeque};
use std::mem::take;
use std::time::SystemTime;

use serde_json::Value;

use crate::background_task::{
    BackgroundTaskDiscoveryState, BackgroundTaskKey, BackgroundTaskRefs, BackgroundTaskRegistry,
    BackgroundTaskSnapshot, BackgroundTaskState, BackgroundTaskTranscriptUpdate,
    BackgroundTaskUpdate,
};
use crate::chat::Item;
use crate::claude_code::sessions::RestoredTask;
use crate::claude_code::tool_items::{complete_tool_item, tool_item};

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

/// The task type of delegated agent work. Background shells, monitors, and
/// workflows travel through the same lifecycle records, so an explicit type is
/// what keeps them out of a view that is about child agents.
const AGENT_TASK_TYPE: &str = "local_agent";

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
}

impl ClaudeTasks {
    pub(crate) fn snapshot(&self) -> Option<BackgroundTaskSnapshot> {
        self.registry.as_ref().map(BackgroundTaskRegistry::snapshot)
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
            if !LAUNCH_TOOLS.contains(&name) {
                continue;
            }
            let Some(tool_use_id) = block["id"].as_str() else {
                continue;
            };

            let input = &block["input"];
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
                    objective: text_field(input, &["prompt", "task", "instructions"]),
                    model: text_field(input, &["model"]),
                    started_at: Some(SystemTime::now()),
                    updated_at: Some(SystemTime::now()),
                    ..BackgroundTaskUpdate::default()
                },
            );
        }
        changed
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
            match block["type"].as_str() {
                Some("tool_result") => {
                    let Some(tool_use_id) = block["tool_use_id"].as_str() else {
                        continue;
                    };
                    // Only a result matching a known launch is a child
                    // outcome; every other tool result belongs to the parent.
                    let Some(canonical) = self.canonical(tool_use_id) else {
                        continue;
                    };
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
                _ => {}
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
        let items = self.child_items(message);
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
    fn child_items(&mut self, message: &Value) -> Vec<Item> {
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
            if !text.trim().is_empty() {
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
        // Every task type shares these records. A record that names a
        // non-agent type is not this view's work, and one that names no type
        // at all is not assumed to be an agent — it may still enrich a row a
        // parent `Task` launch already created.
        let task_type = record["task_type"].as_str();
        if task_type.is_some_and(|task_type| task_type != AGENT_TASK_TYPE) {
            return false;
        }

        let ids = record_identifiers(record);
        let known = ids.iter().any(|id| self.canonical(id).is_some());
        if !known && task_type != Some(AGENT_TASK_TYPE) {
            return false;
        }
        let Some(canonical) = self.canonical_from(&ids) else {
            return false;
        };
        self.link_all(&canonical, &ids);

        let state = lifecycle_state(kind, record);
        let update = BackgroundTaskUpdate {
            refs: Some(refs_from(record)),
            state,
            display_name: text_field(record, &["description"]),
            agent_type: task_type.map(str::to_owned),
            // `summary` is the child's own account of what it did; the last
            // tool it ran is the best live substitute while it is working.
            status: text_field(record, &["summary", "last_tool_name"]),
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
    let content = &block["content"];
    let text = content.as_str().map(str::to_owned).or_else(|| {
        let parts: Vec<&str> = content
            .as_array()?
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect();
        (!parts.is_empty()).then(|| parts.join("\n"))
    })?;
    condense(&text)
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

fn condense(text: &str) -> Option<String> {
    const MAX_PREVIEW_CHARS: usize = 160;
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }
    Some(match text.char_indices().nth(MAX_PREVIEW_CHARS) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text,
    })
}

fn text_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value[*key]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    })
}

#[cfg(test)]
mod tests;
