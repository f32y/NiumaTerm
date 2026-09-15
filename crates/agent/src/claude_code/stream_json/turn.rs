//! Where the CLI is in a turn, as far as its output lines say.
//!
//! The CLI announces a turn's completion with a `result` line but has no start
//! notification, so a sent turn is pending until its first output arrives, and
//! a turn the CLI opens on its own is only visible through that output.

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TurnState {
    Idle,
    Pending,
    Running,
}

/// The running turn and the provider identity it was accepted under.
pub(super) struct TurnTracker {
    state: TurnState,
    accepted: Option<String>,
}

/// What one CLI line said about the turn.
pub(super) struct TurnObservation {
    /// The line opened the turn, which has not been reported as started yet.
    pub(super) started: bool,

    /// The turn it opened was queued by the CLI rather than sent from this
    /// side, so nothing has begun its transcript.
    pub(super) adopted: bool,

    /// The provider identity the turn was accepted under, first stated by
    /// this line.
    pub(super) accepted: Option<String>,
}

impl Default for TurnTracker {
    fn default() -> Self {
        Self {
            state: TurnState::Idle,
            accepted: None,
        }
    }
}

impl TurnTracker {
    pub(super) fn is_idle(&self) -> bool {
        self.state == TurnState::Idle
    }

    #[cfg(test)]
    pub(super) fn state(&self) -> TurnState {
        self.state
    }

    /// Read one line from the CLI.
    ///
    /// A message written while a turn was still running is queued by the CLI
    /// and then run as a turn of its own, opened with no send from this side.
    /// Model output is the only announcement that turn makes, so it has to be
    /// adopted here; otherwise it is never reported as started, and everything
    /// it produces is filed under the turn that preceded it.
    pub(super) fn observe(&mut self, message: &Value) -> TurnObservation {
        let adopted = self.state == TurnState::Idle && carries_model_output(message);

        if adopted {
            self.accepted = None;
        }

        let started = adopted || self.state == TurnState::Pending;

        if started {
            self.state = TurnState::Running;
        }

        TurnObservation {
            started,
            adopted,
            accepted: self.accept(message),
        }
    }

    /// The provider identity of the turn, when `message` is the first line of
    /// it to state one. Child-agent lines carry their own identities, so only
    /// the parent's own lines count.
    fn accept(&mut self, message: &Value) -> Option<String> {
        if self.state == TurnState::Idle
            || self.accepted.is_some()
            || !message["parent_tool_use_id"].is_null()
        {
            return None;
        }

        let provider_id = match message["type"].as_str() {
            Some("stream_event") if message["event"]["type"] == "message_start" => {
                message["event"]["message"]["id"].as_str()
            }
            Some("assistant") => message["message"]["id"].as_str(),
            Some("result") if message["is_error"].as_bool() == Some(false) => {
                message["uuid"].as_str()
            }
            _ => None,
        }?;

        if provider_id.is_empty() {
            return None;
        }

        let id = format!("response:{provider_id}");

        self.accepted = Some(id.clone());

        Some(id)
    }

    /// Open a turn for a user message just written. Returns `false` when a
    /// turn is already running, which the message then steers instead.
    pub(super) fn begin_message_turn(&mut self) -> bool {
        if !self.is_idle() {
            return false;
        }

        self.state = TurnState::Pending;
        self.accepted = None;

        true
    }

    /// Open a turn for a slash command just written. Only called while idle.
    pub(super) fn begin_command_turn(&mut self) {
        self.state = TurnState::Pending;
    }

    /// Close the turn on its `result` line, returning the identity it was
    /// accepted under.
    pub(super) fn finish(&mut self) -> Option<String> {
        self.state = TurnState::Idle;

        self.accepted.take()
    }

    /// Close the turn because the process exited.
    pub(super) fn exit(&mut self) {
        self.state = TurnState::Idle;
    }
}

/// Whether a line is model output, which the CLI only emits inside a turn.
/// The turn's `system`/`init` line arrives first but is also emitted on
/// startup and on resume, where no turn has opened yet.
fn carries_model_output(message: &Value) -> bool {
    matches!(message["type"].as_str(), Some("assistant" | "stream_event"))
}
