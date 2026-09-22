//! Typed payloads of the bridge frames [`crate::dsh::Session::process`]
//! routes on.
//!
//! Each struct names the fields its frame cannot be used without. Parsing
//! them up front replaces the field-by-field `as_str()` probing that decoded
//! a renamed or missing field as a silent empty result: the bridge publishes
//! these frames for this client alone, so a payload that fails to parse is
//! version drift worth a log line. List-shaped fields stay as raw values
//! because their readers are deliberately tolerant per entry — one malformed
//! row drops that row, never the whole catalog.

use serde::Deserialize;
use serde_json::Value;
use tracing::warn;

/// Parse one frame payload, or log why it could not be used.
pub(crate) fn parse<'a, T: Deserialize<'a>>(frame_type: &str, payload: &'a Value) -> Option<T> {
    match T::deserialize(payload) {
        Ok(parsed) => Some(parsed),
        Err(error) => {
            warn!("deepseek {frame_type} frame did not parse: {error}");

            None
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubagentsFrame {
    pub(crate) session_id: String,
    #[serde(default)]
    pub(crate) activity: u64,
    #[serde(default)]
    pub(crate) catalog: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JobsFrame {
    pub(crate) session_id: String,
    #[serde(default)]
    pub(crate) jobs: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubagentTranscriptFrame {
    pub(crate) session_id: String,
    pub(crate) child_session_id: String,
    #[serde(default)]
    pub(crate) page: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkflowTranscriptFrame {
    pub(crate) task_id: String,
    pub(crate) agent_id: String,
    #[serde(default)]
    pub(crate) page: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillsFrame {
    pub(crate) session_id: String,
    #[serde(default)]
    pub(crate) skills: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PresetsFrame {
    pub(crate) session_id: String,
    #[serde(default)]
    pub(crate) presets: Value,
    #[serde(default)]
    pub(crate) current: Option<String>,
    #[serde(default)]
    pub(crate) refusal: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CommandsFrame {
    pub(crate) session_id: String,
    #[serde(default)]
    pub(crate) commands: Value,
}

/// The pending-inbox snapshot. The session id is checked by the dispatch
/// guard before this parses, because a mismatched queue frame falls through
/// to ordinary log-event mapping instead of being dropped.
#[derive(Deserialize)]
pub(crate) struct QueueFrame {
    #[serde(default)]
    pub(crate) items: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReplayFrame {
    pub(crate) session_id: String,
    #[serde(default)]
    pub(crate) error: Option<String>,
    #[serde(default)]
    pub(crate) page: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelsFrame {
    pub(crate) session_id: String,
    #[serde(default)]
    pub(crate) models: Value,
    #[serde(default)]
    pub(crate) error: Option<String>,
    #[serde(default)]
    pub(crate) read_error: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct HistoryFrame {
    #[serde(default)]
    pub(crate) sessions: Value,
    #[serde(default)]
    pub(crate) cwd: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct SearchFrame {
    #[serde(default)]
    pub(crate) error: Option<String>,
    #[serde(default)]
    pub(crate) matches: Value,
    #[serde(default)]
    pub(crate) sessions: Value,
    #[serde(default)]
    pub(crate) cwd: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct ForkCheckpointsFrame {
    #[serde(default)]
    pub(crate) error: Option<String>,
    #[serde(default)]
    pub(crate) page: Value,
}

/// The answer to a command this adapter issued on the session's behalf.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SettledFrame {
    pub(crate) session_id: String,
    pub(crate) command: SettledCommand,
}

/// Commands report only what the stream itself will not: an accepted prompt,
/// rename or queue removal shows up as the log event, title or inbox snapshot
/// it caused, so those carry their failure and nothing else.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum SettledCommand {
    PromptRefused {
        /// Whether the prompt was aimed at a running turn rather than
        /// opening one, which decides what has to be taken back.
        steering: bool,

        error: String,
    },
    #[serde(rename_all = "camelCase")]
    ModelSelected {
        /// The model that was asked for, recorded only once it is in force.
        model: String,

        #[serde(default)]
        reasoning_effort: Option<String>,
        #[serde(default)]
        error: Option<String>,
    },
    Slash {
        name: String,
        arguments: String,
        #[serde(default)]
        value: Value,
        #[serde(default)]
        error: Option<String>,
    },
    QueueRemovalRefused {
        error: String,
    },
    RenameRefused {
        error: String,
    },
    /// A conversation change finished opening; the streams it opened wait in
    /// the session's hand-over slot.
    Switched,
    SwitchFailed {
        error: String,
    },
}
