//! Background shells, the view's other row kind.
//!
//! A shell holds no conversation: it is one command plus the file its output
//! is written to, so what is kept for it is the command line, that file, and
//! whether the row is a shell at all.

use std::collections::{HashMap, VecDeque};

use serde_json::Value;

use crate::background_task::{BackgroundTaskKey, BackgroundTaskKind};
use crate::claude_code::tasks::records::result_content;
use crate::claude_code::tasks::{ClaudeTasks, ShellDetail, ShellMeta};
use crate::json::text_field;

/// Shell entries retained per session, applied to both the metadata table and
/// the command table beside it. Each entry is a few short strings, so this only
/// caps a stream that keeps registering commands.
const MAX_SHELL_META: usize = 256;

/// The output file named in a backgrounded command's handoff result. The file
/// is `<task id>.output`, so that name locates it inside the sentence; the path
/// itself starts at the last `": "` before it, which keeps a directory
/// containing spaces intact where splitting on whitespace would truncate it.
pub(super) fn handoff_output_file(text: &str, task_id: &str) -> Option<String> {
    let name = format!("{task_id}.output");
    let end = text.find(&name)? + name.len();
    let start = text[..end].rfind(": ")? + 2;
    let path = text[start..end].trim();

    (!path.is_empty()).then(|| path.to_owned())
}

/// What the stream has said about each background shell, and the `Bash`
/// commands the shell rows are built from. Both tables are bounded the same
/// way and cleared together, because a command with no shell to attach to is
/// dead weight and a shell with no command has nothing to show.
#[derive(Default)]
pub(super) struct ShellIndex {
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

impl ShellIndex {
    pub(super) fn clear(&mut self) {
        self.shell_meta.clear();
        self.shell_meta_order.clear();
        self.bash_commands.clear();
        self.bash_command_order.clear();
    }

    /// What one shell's records have said about it so far, for a caller that
    /// needs more than the command line.
    pub(super) fn meta(&self, canonical: &str) -> Option<&ShellMeta> {
        self.shell_meta.get(canonical)
    }

    /// Keep what a shell record carries whether or not the shell is
    /// backgrounded yet. `task_started` is the only record naming both the
    /// task and the `Bash` call behind it, so it is the one chance to tie a
    /// row to the command it runs.
    pub(super) fn remember_shell(&mut self, record: &Value) {
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
    pub(super) fn remember_handoff_output_file(&mut self, canonical: &str, block: &Value) {
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
    pub(super) fn remember_output_file(&mut self, record: &Value) {
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

    pub(super) fn remember_bash_command(&mut self, tool_use_id: &str, input: &Value) {
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

    pub(super) fn reserve_shell_meta(&mut self, task_id: &str) {
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

    pub(super) fn shell_command(&self, canonical: &str) -> Option<String> {
        self.shell_meta.get(canonical)?.command.clone()
    }
}

impl ClaudeTasks {
    pub(super) fn is_shell(&self, canonical: &str) -> bool {
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
