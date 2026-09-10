use std::fs;
use std::process::Command;

use serde_json::{Value, json};

use crate::LaunchConfig;
use crate::hook_store::home_dir;

pub(super) const ANTHROPIC_MODEL_ENV: &str = "ANTHROPIC_MODEL";
pub(super) const FILE_CHECKPOINTING_ENV: &str = "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING";

/// The model the CLI must start on. `ANTHROPIC_MODEL` comes first because it
/// is exported into the child environment and would win there anyway; the
/// launch config's own field carries the model the tab asked for otherwise.
pub(super) fn launch_model(launch: &LaunchConfig) -> Option<String> {
    // Command environment overrides are last-value-wins, so the adapter must
    // resolve duplicate entries the same way as the spawned Claude process.
    launch
        .env
        .iter()
        .rev()
        .find(|(name, _)| name.trim().eq_ignore_ascii_case(ANTHROPIC_MODEL_ENV))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            launch
                .model
                .as_deref()
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(str::to_owned)
        })
}

pub(super) fn initial_ready_model(model: Option<&str>) -> String {
    model.unwrap_or("default").to_string()
}

pub(super) fn enable_file_checkpointing(command: &mut Command) {
    command.env(FILE_CHECKPOINTING_ENV, "true");
}

pub(super) fn file_rewind_request(user_message_id: &str) -> Value {
    json!({
        "subtype": "rewind_files",
        "user_message_id": user_message_id,
    })
}

/// The permission mode the CLI will start in, from `~/.claude/settings.json`
/// (`permissions.defaultMode`). The protocol has no way to query the mode
/// before the first turn, so this mirrors the CLI's own config resolution;
/// project-level overrides are not consulted (rare, and the first turn's
/// `init` message corrects any mismatch).
pub(super) fn configured_permission_mode() -> Option<String> {
    let path = home_dir()?.join(".claude").join("settings.json");
    let settings: Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;

    settings["permissions"]["defaultMode"]
        .as_str()
        .map(str::to_owned)
}
