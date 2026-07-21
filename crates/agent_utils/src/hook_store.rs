//! Shared store for hook registrations kept in a user-scope JSON file.
//!
//! Claude Code (`~/.claude/settings.json`) and Codex (`~/.codex/hooks.json`)
//! use the same layout — a root object with a `"hooks"` map of event name to
//! matcher groups, each group holding a `"hooks"` array of command entries —
//! so install/uninstall/status logic lives here once, parametrized by the
//! per-agent event list and entry shape. Only entries whose command references
//! the NiumaTerm hook binary are ever touched, and a file that fails to parse
//! is never rewritten.

use std::io;
use std::path::Path;

use serde_json::{Value, json};

use crate::{AGENT_HOOK_EXE_ENV, HookInstallStatus, hook_command_contains};

/// Markers that identify hook entries owned by NiumaTerm: the current
/// env-var command and legacy installs that baked in an absolute path.
const HOOK_MARKERS: [&str; 2] = [AGENT_HOOK_EXE_ENV, "NiumaTermHook.exe"];

/// Re-registering is idempotent: prior NiumaTerm entries (including legacy
/// absolute-path installs) are removed before `entry(command)` is appended
/// to every event.
pub(crate) fn install_into(
    settings: &mut Value,
    events: &[&str],
    command: &str,
    entry: impl Fn(&str) -> Value,
) -> io::Result<()> {
    uninstall_from(settings);
    let root = settings
        .as_object_mut()
        .expect("hook settings reads only yield objects");
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| invalid("existing \"hooks\" value is not an object"))?;
    for event in events {
        let entries = hooks.entry(*event).or_insert_with(|| json!([]));
        let entries = entries
            .as_array_mut()
            .ok_or_else(|| invalid("existing hook event value is not an array"))?;
        entries.push(entry(command));
    }
    Ok(())
}

pub(crate) fn uninstall_from(settings: &mut Value) {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    for entries in hooks.values_mut() {
        let Some(groups) = entries.as_array_mut() else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(commands) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                commands.retain(|hook| !is_niuma_hook(hook));
            }
        }
        // A matcher group with no commands does nothing; pruning it keeps the
        // file as clean as before the install.
        groups.retain(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|commands| !commands.is_empty())
        });
    }
    hooks.retain(|_, entries| entries.as_array().is_none_or(|groups| !groups.is_empty()));
    if hooks.is_empty() {
        settings
            .as_object_mut()
            .expect("checked above")
            .remove("hooks");
    }
}

pub(crate) fn status_of(settings: &Value, events: &[&str], command: &str) -> HookInstallStatus {
    let marked: Vec<&str> = events
        .iter()
        .flat_map(|event| event_commands(settings, event))
        .filter(|value| is_marked(value))
        .collect();
    if marked.is_empty() {
        HookInstallStatus::NotInstalled
    } else if marked.iter().all(|value| *value == command)
        && events
            .iter()
            .all(|event| event_commands(settings, event).any(|value| value == command))
    {
        HookInstallStatus::Installed
    } else {
        HookInstallStatus::Stale
    }
}

pub(crate) fn event_commands<'a>(
    settings: &'a Value,
    event: &str,
) -> impl Iterator<Item = &'a str> {
    settings
        .pointer(&format!("/hooks/{event}"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|group| group.get("hooks").and_then(Value::as_array))
        .flatten()
        .filter_map(|hook| hook.get("command").and_then(Value::as_str))
}

fn is_niuma_hook(hook: &Value) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(is_marked)
}

pub(crate) fn is_marked(command: &str) -> bool {
    HOOK_MARKERS
        .iter()
        .any(|marker| hook_command_contains(command, marker))
}

/// A missing or empty file reads as an empty object; anything unparseable is
/// surfaced as an error so a broken file is never overwritten. `file_label`
/// names the file in error messages.
pub(crate) fn read(path: &Path, file_label: &str) -> io::Result<Value> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(json!({})),
        Err(error) => return Err(error),
    };
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_str(&text)
        .map_err(|_| invalid(&format!("{file_label} is not valid JSON")))?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(invalid(&format!("{file_label} root is not a JSON object")))
    }
}

/// Write-then-rename so a crash mid-write cannot truncate the user's file.
pub(crate) fn write(path: &Path, settings: &Value) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string_pretty(settings).map_err(io::Error::other)?;
    text.push('\n');
    let temp = path.with_extension("json.niumaterm-tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, path)
}

pub(crate) fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
