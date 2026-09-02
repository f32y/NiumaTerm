//! Every message the stream carries, routed to the child state it belongs to.
//!
//! Claude describes one child across several record shapes arriving in no
//! fixed order, so each shape is taken as it comes and the child it names is
//! opened if this is the first record to mention it.

use std::time::SystemTime;

use serde_json::Value;

use crate::background_task::{
    BackgroundTaskKey, BackgroundTaskKind, BackgroundTaskRefs, BackgroundTaskState,
    BackgroundTaskTranscriptUpdate, BackgroundTaskUpdate,
};
use crate::chat::Item;
use crate::claude_code::tasks::records::{result_text, sidechain_preview};
use crate::claude_code::tasks::{ClaudeTasks, LAUNCH_TOOLS, LIFECYCLE_RECORDS, SHELL_TASK_TYPE};
use crate::claude_code::tool_items::{complete_tool_item, tool_item};
use crate::json::text_field;

impl ClaudeTasks {
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

    pub(super) fn observe_system(&mut self, message: &Value) -> bool {
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
    pub(super) fn advance_epoch(&mut self) -> bool {
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
    pub(super) fn observe_hook(&mut self, message: &Value) -> bool {
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
    pub(super) fn observe_parent_assistant(&mut self, message: &Value) -> bool {
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
    pub(super) fn open_child_conversation(
        &mut self,
        tool_use_id: &str,
        objective: Option<String>,
    ) -> bool {
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
    pub(super) fn observe_parent_user(&mut self, message: &Value) -> bool {
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
    pub(super) fn observe_sidechain(&mut self, parent_tool_use_id: &str, message: &Value) -> bool {
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
    pub(super) fn child_items(&mut self, canonical: &str, message: &Value) -> Vec<Item> {
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

    /// Admit the background shells the CLI currently reports as running. This
    /// is the only record stating that a command already under way has moved
    /// to the background, which is how a `Bash` call the CLI backgrounds on a
    /// timeout reaches the view at all.
    pub(super) fn observe_background_snapshot(&mut self, message: &Value) -> bool {
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
}
