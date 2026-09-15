use std::env;
use std::fs;
use std::process::Command;

use serde_json::{Value, json};

use crate::LaunchConfig;
use crate::hook_store::home_dir;
use crate::launcher::AgentCli;
use crate::workspace::AgentWorkspace;

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

/// Assemble the CLI invocation for one conversation. Kept apart from the spawn
/// so the exact argument boundaries can be inspected without starting a
/// process: a path pushed as its own argument is never re-parsed, which is what
/// keeps a directory containing spaces or shell metacharacters intact.
pub(super) fn claude_command(
    launcher: &AgentCli,
    launch: &LaunchConfig,
    workspace: &AgentWorkspace,
    resume: Option<&str>,
    initial_model: &Option<String>,
) -> Command {
    let mut command = launcher.command([
        "-p",
        "--output-format",
        "stream-json",
        "--input-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-prompt-tool",
        "stdio",
        "--allow-dangerously-skip-permissions",
    ]);

    // File snapshots are opt-in for stream-json SDK clients. This is
    // applied after profile overrides so every NiumaTerm Claude session
    // can create checkpoints for subsequent `/rewind` operations.
    enable_file_checkpointing(&mut command);

    // Recent models omit checklist tools unless the client opts in. Keep an
    // explicit profile or inherited choice while enabling progress by default.
    if !launch
        .env
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("CLAUDE_CODE_ENABLE_TODO_TOOLS"))
        && env::var_os("CLAUDE_CODE_ENABLE_TODO_TOOLS").is_none()
    {
        command.env("CLAUDE_CODE_ENABLE_TODO_TOOLS", "1");
    }

    // Recent models omit thinking text by default and emit signature-only
    // thinking blocks, which would leave the chat's reasoning sections
    // permanently empty. Asking for the summarized form at launch is the only
    // way to get that text for the whole session; the per-session control
    // request only overrides a mode that was already chosen here.
    command.args(["--thinking-display", "summarized"]);

    // The CLI takes effort as a launch flag; its `/effort` command is the
    // only other way in, and that costs a visible turn on every new
    // conversation.
    if let Some(effort) = &launch.effort {
        command.args(["--effort", effort]);
    }

    // The CLI resolves the model once during its handshake and builds the
    // system prompt from it, including the identity it states to the model
    // itself. A later `set_model` reroutes the requests while that prompt
    // keeps describing the startup model, so a model the tab already knows
    // about has to arrive as a launch flag. Without one the CLI starts on
    // the model from its own configuration.
    if let Some(model) = initial_model {
        command.args(["--model", model]);
    }

    if let Some(session_id) = resume {
        command.args(["--resume", session_id]);
    }

    // Additional workspace directories reach the CLI through its own
    // `--add-dir` flag, applied to new and resumed conversations alike. The
    // primary directory is not repeated because the process already starts
    // there, and Claude keeps its session storage and configuration discovery
    // anchored on that directory.
    if workspace.is_multi_root() {
        command.arg("--add-dir");

        command.args(workspace.additional());
    }

    if let Some(cwd) = workspace.primary() {
        command.current_dir(cwd);
    }

    command
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
