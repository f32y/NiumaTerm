//! Claude Code stream-json chat session: process lifecycle, control-protocol
//! handshake, and translation of the backend protocol into typed events for a
//! chat UI.
//!
//! The protocol is the one the official Claude Agent SDK speaks to the CLI:
//! `claude -p --input-format stream-json --output-format stream-json` with
//! newline-delimited JSON both ways. One `Session` owns one long-lived
//! `claude` process; multi-turn conversation is more user messages on stdin.
//! Permission prompts arrive as `control_request { can_use_tool }` because we
//! pass `--permission-prompt-tool stdio` (verified: approvals still fire with
//! `--allow-dangerously-skip-permissions` present — that flag only unlocks
//! switching into `bypassPermissions` mode).

mod control;
mod launch;
mod parse;
mod transcript;

#[cfg(test)]
mod tests;

#[cfg(all(test, windows))]
use std::fs;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::{Value, json};

use crate::LaunchConfig;
use crate::background_task::{BackgroundTaskKey, BackgroundTaskTranscriptUpdate};
#[cfg(test)]
use crate::chat::ContextUsageScope;
use crate::chat::{
    Event, MessageImage, SendOutcome, SlashCommandArguments, SlashCommandInfo, SlashCommandOutcome,
    SlashCommandRunPolicy, SlashCommandSource, ThreadSettings,
};
use crate::claude_code::sessions::{RestoredTask, load_child_transcript};
use crate::claude_code::shell_output::shell_items;
use crate::claude_code::stream_json::control::{
    ControlState, PendingApproval, PendingControlOperation, PendingQuestions,
    merge_question_answers, parse_questions,
};
#[cfg(test)]
use crate::claude_code::stream_json::control::{
    fail_pending_control_operations, resolve_pending_control_operation,
};
#[cfg(test)]
use crate::claude_code::stream_json::launch::{ANTHROPIC_MODEL_ENV, FILE_CHECKPOINTING_ENV};
use crate::claude_code::stream_json::launch::{
    configured_permission_mode, enable_file_checkpointing, file_rewind_request,
    initial_ready_model, launch_model,
};
#[cfg(test)]
use crate::claude_code::stream_json::parse::parse_claude_usage;
use crate::claude_code::stream_json::parse::{
    approval_description, claude_result_error, compaction_progress, initialize_command_catalog,
    legacy_command_catalog, parse_models, slash_command_text, ui_owns_slash_command,
};
#[cfg(test)]
use crate::claude_code::stream_json::parse::{
    context_window_usage, parse_slash_commands, update_claude_output,
};
use crate::claude_code::stream_json::transcript::TranscriptState;
#[cfg(test)]
use crate::claude_code::stream_json::transcript::{TurnOutputUsage, window_from_composition};
use crate::claude_code::tasks::ClaudeTasks;
#[cfg(test)]
use crate::claude_code::tool_items::{edit_diff, input_detail, tool_item};
use crate::claude_code::workflows::{ClaudeWorkflowSource, ClaudeWorkflows};
use crate::deadline_timer::DeadlineTimer;
use crate::launcher::AgentCli;
use crate::request_policy::RequestClass;
use crate::subprocess::JsonLineProcess;
use crate::workflow::{
    RestoredWorkflowRun, WorkflowRefreshRequest, WorkflowRefreshResult, WorkflowSource,
};
use crate::workspace::AgentWorkspace;

const TIMEOUT_METHOD: &str = "nmt/claudeRequestDeadline";

/// Effort level standing for Claude Code's ultracode mode. The CLI does not
/// take it as a level: it is xhigh effort plus standing dynamic-workflow
/// orchestration, carried by a separate session flag. Passing the word as a
/// level would be aliased back to plain xhigh with the orchestration off, so
/// the adapter splits it into the two settings the CLI expects.
pub const ULTRACODE_EFFORT: &str = "ultracode";

/// Serialized values for `--permission-mode` / the `set_permission_mode` control
/// request. `auto` is the CLI's dynamic mode (verified accepted by
/// `set_permission_mode` on 2.1.222).
pub const PERMISSION_OPTIONS: [&str; 5] = [
    "default",
    "auto",
    "acceptEdits",
    "plan",
    "bypassPermissions",
];

const INIT_REQUEST_ID: &str = "nmt-init";
const SESSION_TITLE_DESCRIPTION_CHARS: usize = 2_000;

fn session_title_description(description: &str) -> String {
    description
        .trim()
        .chars()
        .take(SESSION_TITLE_DESCRIPTION_CHARS)
        .collect()
}

pub struct Session {
    process: JsonLineProcess,
    transcript: TranscriptState,
    control: ControlState,
    ready: bool,

    /// The CLI's session id from the `init` message; the handle a future tab
    /// needs to `--resume` this conversation.
    session_id: Option<String>,

    turn_active: bool,

    /// The turn was started locally but no output has arrived yet; the first
    /// message after a send emits `TurnStarted` (the protocol has no explicit
    /// turn-started notification — `result` is the only turn boundary).
    turn_reported: bool,

    accepted_identity: Option<String>,

    /// Model and permission selections sent to the backend. Effort changes
    /// additionally need ordered confirmation and are owned by control state.
    applied_model: Option<String>,

    applied_permission: Option<String>,
    active_slash_command: Option<String>,

    /// A structured initialize catalog carries richer metadata than the
    /// string-only first-turn fallback and must remain authoritative.
    structured_commands_published: bool,

    /// A compaction is running. Tracked because the CLI re-announces it every
    /// 30 seconds while a long compaction proceeds, and the UI only needs the
    /// state transitions.
    compacting: bool,

    /// Child-agent state reduced from Task launches, lifecycle records, and
    /// linked sidechain traffic for the `Background Tasks` view.
    tasks: ClaudeTasks,

    /// Workflow runs, reduced from the same records the child-agent reducer
    /// rejects. The two views never share a row.
    workflows: ClaudeWorkflows,

    workflow_source: Arc<dyn WorkflowSource>,
}

impl Session {
    /// Commands implemented by the Claude CLI but not necessarily included
    /// in every version's dynamic discovery payload.
    pub fn adapter_commands() -> Vec<SlashCommandInfo> {
        vec![
            SlashCommandInfo {
                name: "compact".to_string(),
                description: "Compact the current conversation context".to_string(),
                argument_hint: Some("[instructions]".to_string()),
                source: SlashCommandSource::Adapter,
                // The CLI accepts optional instructions steering what the
                // summary keeps, and this adapter forwards whatever it is
                // given as the command's text. This entry exists only as a
                // fallback for versions whose discovery payload omits the
                // command, so declaring no arguments here would reject input
                // the CLI itself accepts.
                arguments: SlashCommandArguments::Freeform,
                run_policy: SlashCommandRunPolicy::QueueUntilIdle,
            },
            SlashCommandInfo {
                name: "rewind".to_string(),
                description: "Restore files or conversation to an earlier prompt".to_string(),
                argument_hint: None,
                source: SlashCommandSource::Adapter,
                arguments: SlashCommandArguments::None,
                run_policy: SlashCommandRunPolicy::IdleOnly,
            },
        ]
    }
}

/// Assemble the CLI invocation for one conversation. Kept apart from the spawn
/// so the exact argument boundaries can be inspected without starting a
/// process: a path pushed as its own argument is never re-parsed, which is what
/// keeps a directory containing spaces or shell metacharacters intact.
fn claude_command(
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

impl Session {
    /// Spawn `claude` in bidirectional stream-json mode and send the SDK-style
    /// `initialize` control request. Every parsed stdout line is handed to
    /// `deliver` (from a reader thread — hop threads before calling
    /// [`Session::process`]); stderr lines go to `on_stderr`.
    ///
    /// With `resume`, the CLI reloads that persisted session and appends to
    /// it (same session id, same transcript file). Resume lookup is scoped to
    /// the project directory derived from `cwd`, so the id must come from a
    /// listing for the same directory. Nothing is replayed by the backend — the
    /// UI pre-fills its transcript from the session file instead.
    pub fn spawn(
        launch: &LaunchConfig,
        workspace: &AgentWorkspace,
        resume: Option<String>,
        deliver: impl Fn(Value) + Send + Sync + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        let initial_model = launch_model(launch);
        let launcher = AgentCli::from_launch(launch, "claude");
        let executable = launcher.executable().to_string();

        let command = claude_command(
            &launcher,
            launch,
            workspace,
            resume.as_deref(),
            &initial_model,
        );

        let deliver = Arc::new(deliver);
        let timer_delivery = Arc::clone(&deliver);

        let timer = DeadlineTimer::new(move || {
            timer_delivery(json!({"method": TIMEOUT_METHOD}));
        })
        .map_err(|error| format!("could not start Claude deadline timer: {error}"))?;

        let stop = timer.handle();

        let process = JsonLineProcess::spawn_with_stdout_closed(
            command,
            &executable,
            "Claude",
            move |message| deliver(message),
            on_stderr,
            move || stop.stop(),
        )?;

        let mut session = Self {
            process,
            transcript: TranscriptState::default(),
            control: ControlState::with_effort(launch.effort.clone()),
            ready: false,
            // A resumed process may not emit `system/init` until its next
            // model turn. The caller already obtained this identity from the
            // same cwd's history, so it is immediately valid for local
            // checkpoint lookup; a later init can still confirm or replace it.
            session_id: resume,
            turn_active: false,
            turn_reported: false,
            accepted_identity: None,
            applied_model: initial_model,
            applied_permission: None,
            active_slash_command: None,
            structured_commands_published: false,
            compacting: false,
            tasks: ClaudeTasks::default(),
            workflows: ClaudeWorkflows::default(),
            workflow_source: Arc::new(ClaudeWorkflowSource::default()),
        };

        session.control.set_timer(timer);

        session.try_send(json!({
            "type": "control_request",
            "request_id": INIT_REQUEST_ID,
            "request": {
                "subtype": "initialize",
                // Subagents otherwise report only their tool calls, so a task
                // running under the Agent tool shows as a silent gap. This
                // forwards their text and thinking blocks under the spawning
                // tool-use id so the nested transcript can be rendered.
                "forwardSubagentText": true,
            },
        }))?;

        session.control.record_admitted(
            INIT_REQUEST_ID.to_string(),
            RequestClass::Query,
            Instant::now(),
        );

        Ok(session)
    }

    pub fn has_active_operation(&self) -> bool {
        self.turn_active || self.control.has_active_request() || self.compacting
    }

    /// Request EOF shutdown and wait for the launcher plus every contained
    /// descendant. Forced termination is used only after an explicit user
    /// choice to interrupt active work.
    pub fn shutdown(&mut self, timeout: Duration, force: bool) -> Result<(), String> {
        if force {
            // Forced closure retires requests before EOF can drain queued
            // side effects. Graceful shutdown still drains accepted input.
            self.on_exit();
        }

        self.process.shutdown(timeout, force)
    }

    /// Handle one message from the CLI: answers control requests and returns
    /// the events a chat UI reacts to.
    pub fn process(&mut self, message: Value) -> Vec<Event> {
        if self.control.is_closed() {
            return Vec::new();
        }

        if message["method"] == TIMEOUT_METHOD {
            return self.poll_timeouts(Instant::now());
        }

        let mut events = Vec::new();

        // Child reduction runs before parent handling because the parent path
        // drops linked sidechain records to keep child text out of the
        // transcript; the child state would otherwise be lost with them.
        let tasks_changed = self.tasks.observe(&message);
        let workflows_changed = self.workflows.observe(&message);

        // A message written while a turn was still running is queued by the
        // CLI and then run as a turn of its own, opened with no send from this
        // side. Model output is the only announcement that turn makes, so it
        // has to be adopted here; otherwise it is never reported as started,
        // and everything it produces is filed under the turn that preceded it.
        if !self.turn_active && carries_model_output(&message) {
            self.turn_active = true;
            self.turn_reported = false;
            self.accepted_identity = None;
            self.transcript.begin_turn();
        }

        // First sign of life after a send: the turn is actually running.
        if self.turn_active && !self.turn_reported {
            self.turn_reported = true;
            events.push(Event::TurnStarted);
        }

        if self.turn_active
            && self.accepted_identity.is_none()
            && message["parent_tool_use_id"].is_null()
        {
            let provider_id = match message["type"].as_str() {
                Some("stream_event") if message["event"]["type"] == "message_start" => {
                    message["event"]["message"]["id"].as_str()
                }

                Some("assistant") => message["message"]["id"].as_str(),

                Some("result") if message["is_error"].as_bool() == Some(false) => {
                    message["uuid"].as_str()
                }

                _ => None,
            };

            if let Some(id) = provider_id.filter(|id| !id.is_empty()) {
                let id = format!("response:{id}");

                self.accepted_identity = Some(id.clone());
                events.push(Event::ProviderTurnAccepted { id });
            }
        }

        match message["type"].as_str() {
            Some("system") => events.extend(self.on_system(&message)),
            Some("stream_event") => events.extend(self.transcript.on_stream_event(&message)),
            Some("assistant") => events.extend(self.transcript.on_assistant(&message)),
            Some("user") => events.extend(self.transcript.on_tool_results(&message)),
            Some("result") => events.extend(self.on_result(&message)),
            Some("control_request") => events.extend(self.on_control_request(&message)),
            Some("control_response") => events.extend(self.on_control_response(&message)),

            Some("control_cancel_request") => {
                if let Some(id) = message["request_id"].as_str() {
                    events.extend(self.control.cancel_prompt(id));
                }
            }

            _ => {}
        }

        if tasks_changed && let Some(snapshot) = self.tasks.snapshot() {
            events.push(Event::BackgroundTasks(snapshot));
        }

        if workflows_changed && let Some(snapshot) = self.workflows.snapshot() {
            events.push(Event::Workflows(snapshot));
        }

        // A child's own conversation travels separately from its summary; the
        // parent transcript above has already dropped this content.
        for (key, update) in self.tasks.take_transcripts() {
            events.push(Event::BackgroundTaskTranscript { key, update });
        }

        events
    }

    /// Write the user message, applying changed settings first via control
    /// requests (model, permission mode and effort are session state on the
    /// CLI, so they are set once per change instead of per turn).
    /// Send a user message carrying `images`, which the CLI takes inline as
    /// content blocks beside the text; it has no path input.
    pub fn send_user_message(
        &mut self,
        text: &str,
        settings: &ThreadSettings,
        images: &[MessageImage],
    ) -> SendOutcome {
        if self.control.is_closed() || !self.process.has_stdin() {
            return SendOutcome::NotReady;
        }

        let mut messages = Vec::new();

        if settings.model.is_some() && settings.model != self.applied_model {
            let model = settings.model.clone().unwrap_or_default();

            messages.push(
                self.control
                    .request(json!({"subtype": "set_model", "model": model}))
                    .1,
            );
        }

        if settings.approval.is_some() && settings.approval != self.applied_permission {
            let approval = settings.approval.clone().unwrap_or_default();

            messages.push(
                self.control
                    .request(json!({"subtype": "set_permission_mode", "mode": approval}))
                    .1,
            );
        }

        let mut pending_effort = None;

        if settings.effort.is_some() && settings.effort.as_deref() != self.control.effort() {
            let effort = settings.effort.clone().unwrap_or_default();
            let ultracode = effort == ULTRACODE_EFFORT;

            let level = if ultracode { "xhigh" } else { effort.as_str() };

            // A refusal must restore the previous effort selection, unlike
            // model and permission errors that only report a diagnostic.
            let (request_id, request) = self.control.request(json!({
                "subtype": "apply_flag_settings",
                "settings": {"effortLevel": level, "ultracode": ultracode},
            }));

            messages.push(request);
            pending_effort = Some((request_id, effort));
        }

        let mut content = vec![json!({"type": "text", "text": text})];

        content.extend(images.iter().map(|image| {
            json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": image.media_type,
                    "data": BASE64_STANDARD.encode(&image.bytes),
                },
            })
        }));

        messages.push(json!({
            "type": "user",
            "message": {"role": "user", "content": content},
        }));

        let control_ids: Vec<String> = messages
            .iter()
            .filter_map(|message| message["request_id"].as_str().map(str::to_owned))
            .collect();

        if let Err(message) = self.control.check_connected() {
            return SendOutcome::Rejected { message };
        }

        let ticket = match self.process.write_tracked(messages) {
            Ok(ticket) => ticket,

            Err(error) => {
                return SendOutcome::Rejected {
                    message: error.to_string(),
                };
            }
        };

        for id in control_ids {
            self.control
                .record_admitted(id.clone(), RequestClass::Mutation, Instant::now());

            self.control.attach_input(&id, ticket.clone());
            self.control.track(id, PendingControlOperation::Other);
        }

        if settings.model.is_some() {
            self.applied_model = settings.model.clone();
        }

        if settings.approval.is_some() {
            self.applied_permission = settings.approval.clone();
        }

        if let Some((request_id, effort)) = pending_effort {
            self.control.record_effort(request_id, effort);
        }

        if self.turn_active {
            SendOutcome::Steered
        } else {
            self.turn_active = true;
            self.turn_reported = false;
            self.accepted_identity = None;
            self.transcript.begin_turn();

            SendOutcome::StartedTurn
        }
    }

    /// Send a provider command through Claude's stream-json command path.
    /// This intentionally bypasses `send_user_message`: the UI must not add
    /// a user bubble or steer a running model turn for slash commands.
    pub fn execute_slash_command(&mut self, name: &str, arguments: &str) -> SlashCommandOutcome {
        if ui_owns_slash_command(name) {
            return SlashCommandOutcome::Rejected {
                message: format!("/{name} is handled by NiumaTerm."),
            };
        }

        if !self.ready || !self.process.has_stdin() {
            return SlashCommandOutcome::NotReady;
        }

        if self.turn_active {
            return SlashCommandOutcome::Rejected {
                message: "Claude is already running a turn.".to_string(),
            };
        }

        let text = slash_command_text(name, arguments);

        if let Err(error) = self.process.try_write_line(json!({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "text", "text": text}]},
        })) {
            return SlashCommandOutcome::Rejected {
                message: error.to_string(),
            };
        }

        self.turn_active = true;
        self.turn_reported = false;
        self.transcript.begin_turn();
        self.active_slash_command = Some(name.to_string());

        SlashCommandOutcome::Accepted
    }

    /// Restore files tracked by Claude to the state captured before the user
    /// message. Completion arrives asynchronously as `FileRewindCompleted`.
    /// Ask the CLI how the context window is currently filled. This is a local
    /// computation rather than a model call, so it is cheap enough to refresh
    /// whenever the conversation grows; the answer arrives as an event.
    pub fn request_context_composition(&mut self) -> bool {
        if !self.ready || !self.process.has_stdin() {
            return false;
        }

        // One outstanding request is enough: a second would answer with the
        // same breakdown the first is already about to deliver.
        if self
            .control
            .contains(&PendingControlOperation::ContextComposition)
        {
            return false;
        }

        let Ok(request_id) = self.send_control(json!({"subtype": "get_context_usage"})) else {
            return false;
        };

        self.control
            .track(request_id, PendingControlOperation::ContextComposition);

        true
    }

    /// Ask the CLI to name this conversation. The CLI summarizes `description`
    /// with a model call, so the answer is a name for the subject rather than
    /// a truncation of the prompt, and it arrives later as
    /// [`Event::TitleUpdated`]. `persist` writes the name into the session
    /// file, so resuming the conversation finds it under the same name.
    ///
    /// The CLI answers with a null title when `description` is under ten
    /// characters, and a build without this request answers with an error;
    /// both leave the conversation unnamed rather than reporting anything.
    pub fn request_session_title(&mut self, description: &str) -> bool {
        if !self.ready || !self.process.has_stdin() {
            return false;
        }

        // One outstanding request is enough: a second would name the same
        // conversation twice.
        if self
            .control
            .contains(&PendingControlOperation::SessionTitle)
        {
            return false;
        }

        let description = session_title_description(description);

        if description.is_empty() {
            return false;
        }

        let Ok(request_id) = self.send_control(json!({
            "subtype": "generate_session_title",
            "description": description,
            "persist": true,
        })) else {
            return false;
        };

        self.control
            .track(request_id, PendingControlOperation::SessionTitle);

        true
    }

    /// Give the conversation the name the user typed. The CLI records it with
    /// the session, so its own listings show it instead of a title derived
    /// from the opening prompt -- and so does this application's, which reads
    /// the same files.
    ///
    /// Fire-and-forget, like the model and permission requests: a refusal
    /// reaches the user through the generic control-error path, and there is
    /// nothing to put back when a name the tab already carries is rejected.
    pub fn rename_session(&mut self, title: &str) -> bool {
        let title = title.trim();

        if !self.ready || !self.process.has_stdin() || title.is_empty() {
            return false;
        }

        if self
            .send_control(json!({"subtype": "rename_session", "title": title}))
            .is_err()
        {
            return false;
        }

        self.control.cancel_generated_title();

        true
    }

    pub fn rewind_files(&mut self, user_message_id: &str) -> SlashCommandOutcome {
        if !self.ready || !self.process.has_stdin() {
            return SlashCommandOutcome::NotReady;
        }

        if self.turn_active || self.control.pending_approval.is_some() {
            return SlashCommandOutcome::Rejected {
                message: "Claude must be idle before restoring files.".to_string(),
            };
        }

        if self.control.contains(&PendingControlOperation::FileRewind) {
            return SlashCommandOutcome::Rejected {
                message: "A Claude file restore is already running.".to_string(),
            };
        }

        let request_id = match self.send_control(file_rewind_request(user_message_id)) {
            Ok(id) => id,
            Err(message) => return SlashCommandOutcome::Rejected { message },
        };

        self.control
            .track(request_id, PendingControlOperation::FileRewind);

        SlashCommandOutcome::Accepted
    }

    /// Resolve operations that can no longer receive a control response after
    /// stdout closes. The pane calls this before reporting the process exit.
    /// Start rebuilding child agents from this session's persisted history.
    /// The returned order counter must be passed back to
    /// [`Session::finish_task_restoration`] so a slow read cannot overwrite
    /// live updates that landed while it was running.
    pub fn begin_task_restoration(&mut self) -> u64 {
        self.tasks.begin_restoration()
    }

    /// Read one child's stored conversation. The CLI publishes a child's own
    /// turns only in the file it writes for that child; the parent stream
    /// carries the launch instruction and the lifecycle records but none of
    /// the replies, so this read is what the child's row has to show.
    ///
    /// A session that wrote no child files leaves the row as the stream left
    /// it, which keeps older CLI versions (whose stream did carry the child's
    /// turns) working unchanged.
    pub fn load_background_task_transcript(
        &self,
        tool_use_id: &str,
        cwd: Option<&str>,
    ) -> Vec<Event> {
        // A background shell keeps its content in an output file rather than
        // in a child session, so it answers from the reducer and never looks
        // for a transcript that does not exist.
        if let Some(detail) = self.tasks.shell_detail(tool_use_id) {
            return vec![Event::BackgroundTaskTranscript {
                key: BackgroundTaskKey::claude_code(tool_use_id),
                update: BackgroundTaskTranscriptUpdate::loaded(shell_items(&detail)),
            }];
        }

        let Some(session_id) = self.session_id.as_deref() else {
            return Vec::new();
        };

        let Some(items) = load_child_transcript(cwd, session_id, tool_use_id) else {
            return Vec::new();
        };

        vec![Event::BackgroundTaskTranscript {
            key: BackgroundTaskKey::claude_code(tool_use_id),
            update: BackgroundTaskTranscriptUpdate::loaded(items),
        }]
    }

    pub fn finish_task_restoration(
        &mut self,
        restored: Result<Vec<RestoredTask>, String>,
        starting_sequence: u64,
    ) -> Vec<Event> {
        let changed = self.tasks.finish_restoration(restored, starting_sequence);

        // The restored child conversations must be published even when no row
        // changed: a summary that already matched still leaves this the only
        // delivery of the conversations behind it.
        let mut events: Vec<Event> = self
            .tasks
            .take_transcripts()
            .into_iter()
            .map(|(key, update)| Event::BackgroundTaskTranscript { key, update })
            .collect();

        if changed && let Some(snapshot) = self.tasks.snapshot() {
            events.push(Event::BackgroundTasks(snapshot));
        }

        events
    }

    /// Expire unanswered protocol requests without retrying side effects.
    pub fn poll_timeouts(&mut self, now: Instant) -> Vec<Event> {
        let mut events = Vec::new();

        for (id, class, ticket) in self.control.expired(now) {
            let cancelled = ticket.as_ref().is_some_and(|ticket| ticket.cancel());

            let message = if cancelled {
                "Claude request expired before writing and was cancelled; it was not sent."
                    .to_string()
            } else {
                class.timeout_message("Claude")
            };

            if cancelled && ticket.as_ref().is_some_and(|ticket| ticket.is_batch()) {
                self.process.abort();
                events.extend(self.control.close(&message));
                events.push(Event::Error { message: format!("{message} The entire settings-and-prompt batch was cancelled. Reopen the session before retrying."), fatal: true });

                break;
            }

            if id == INIT_REQUEST_ID {
                events.extend(self.control.close(&message));

                events.push(Event::Error {
                    message,
                    fatal: true,
                });

                break;
            }

            if !cancelled && let Some(effort_events) = self.control.expire_effort(&id) {
                events.extend(effort_events);

                events.push(Event::Error {
                    message,
                    fatal: false,
                });

                continue;
            }

            let result = self.on_control_response(
                &json!({"response": {"request_id": id, "subtype": "error", "error": message}}),
            );

            if result.is_empty() {
                events.push(Event::Error {
                    message,
                    fatal: false,
                });
            } else {
                events.extend(result);
            }
        }

        events
    }

    pub fn on_exit(&mut self) -> Vec<Event> {
        self.ready = false;
        self.turn_active = false;
        self.turn_reported = false;

        let message = "Claude exited before the control request completed.";
        let mut events = self.control.close(message);

        if self.compacting {
            self.compacting = false;

            events.push(Event::CompactionFinished {
                error: Some(message.to_string()),
            });
        }

        if let Some(name) = self.active_slash_command.take() {
            events.push(Event::SlashCommandResult {
                name,
                outcome: SlashCommandOutcome::Rejected {
                    message: message.to_string(),
                },
            });
        }

        events
    }

    /// Interrupt the running turn (the Esc/Ctrl-C equivalent).
    pub fn interrupt(&mut self) -> bool {
        self.send_control(json!({"subtype": "interrupt"})).is_ok()
    }

    /// Stop one child agent, leaving this session's own turn running. Returns
    /// whether the request went out: a row the stream has not yet given a task
    /// id names nothing the CLI's task registry could find.
    ///
    /// The CLI answers a task it cannot find, or one already settled, as a
    /// success, so the response says only that the request was understood. The
    /// child's own lifecycle record is what reports it stopped.
    pub fn interrupt_background_task(&mut self, key: &BackgroundTaskKey) -> bool {
        let Some(task_id) = self.tasks.stop_target(key).map(str::to_owned) else {
            return false;
        };

        self.send_control(json!({"subtype": "stop_task", "task_id": task_id}))
            .is_ok()
    }

    /// The CLI's session id, known immediately for a resumed process and
    /// otherwise populated by its `init` message. This is what `spawn`'s
    /// `resume` takes to reopen the conversation later.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Answer the pending `can_use_tool` request. The UI decision vocabulary
    /// maps onto the CLI's allow/deny responses: `accept` allows once,
    /// `acceptForSession` allows and applies the CLI's own permission
    /// suggestions (e.g. switching to acceptEdits for the session), `decline`
    /// denies, and `cancel` denies and interrupts the turn.
    pub fn respond_approval(&mut self, decision: &str) -> bool {
        let Some(pending) = self.control.pending_approval.as_ref() else {
            return false;
        };

        let response = match decision {
            "accept" => json!({"behavior": "allow", "updatedInput": pending.input}),

            "acceptForSession" => {
                let mut response = json!({"behavior": "allow", "updatedInput": pending.input});

                if let Some(suggestions) = &pending.suggestions {
                    response["updatedPermissions"] = suggestions.clone();
                }

                response
            }

            "cancel" => json!({"behavior": "deny", "message": "User cancelled tool execution."}),
            _ => json!({"behavior": "deny", "message": "User declined tool execution."}),
        };

        if self
            .try_send(json!({
                "type": "control_response",
                "response": {
                    "subtype": "success",
                    "request_id": pending.request_id,
                    "response": response,
                },
            }))
            .is_err()
        {
            return false;
        }

        self.control.pending_approval = None;

        if decision == "cancel" {
            self.interrupt();
        }

        true
    }

    /// Answer the pending `AskUserQuestion` request. `answers` holds the chosen
    /// option labels per question, in question order; `None` declines. The CLI
    /// re-runs the tool with the merged input and writes the tool result
    /// itself, so nothing further is sent for this tool call.
    pub fn respond_questions(&mut self, answers: Option<Vec<Vec<String>>>) -> bool {
        let Some(pending) = self.control.pending_questions.as_ref() else {
            return false;
        };

        let response = match answers {
            Some(answers) if answers.iter().any(|labels| !labels.is_empty()) => {
                let updated_input =
                    merge_question_answers(pending.input.clone(), &pending.questions, answers);

                json!({"behavior": "allow", "updatedInput": updated_input})
            }

            // Declining is a deny, which the CLI turns into a "no answer"
            // tool result; the turn continues instead of aborting.
            _ => json!({
                "behavior": "deny",
                "message": "The user dismissed the questions; make your best assumption and continue.",
            }),
        };

        if self
            .try_send(json!({
                "type": "control_response",
                "response": {
                    "subtype": "success",
                    "request_id": pending.request_id,
                    "response": response,
                },
            }))
            .is_err()
        {
            return false;
        }

        self.control.pending_questions = None;

        true
    }

    /// The background reader shares this session's transcript revisions.
    pub fn workflow_source(&self) -> Arc<dyn WorkflowSource> {
        self.workflow_source.clone()
    }

    /// What each still-running workflow run needs read on the next refresh
    /// tick. A terminal run is left out: its record can no longer change.
    pub fn workflow_refresh_requests(&self) -> Vec<WorkflowRefreshRequest> {
        let Some(snapshot) = self.workflows.snapshot() else {
            return Vec::new();
        };

        snapshot
            .runs
            .into_iter()
            .filter(|run| !run.state.is_terminal())
            .map(|run| WorkflowRefreshRequest {
                task_id: run.task_id,
                agent_ids: run
                    .agents
                    .into_iter()
                    .filter_map(|agent| agent.agent_id)
                    .collect(),
                open_agent: None,
                transcript_revision: None,
            })
            .collect()
    }

    /// Fold one run's refresh back in. The transcript travels as its own event
    /// because it is read only while someone has that agent open.
    pub fn apply_workflow_refresh(&mut self, result: WorkflowRefreshResult) -> Vec<Event> {
        let mut events = Vec::new();
        let task_id = result.task_id;

        if self.workflows.apply_refresh(&task_id, result.refresh)
            && let Some(snapshot) = self.workflows.snapshot()
        {
            events.push(Event::Workflows(snapshot));
        }

        if let Some(transcript) = result.transcript {
            events.push(Event::WorkflowAgentTranscript {
                task_id,
                agent_id: transcript.agent_id,
                items: transcript.items,
            });
        }

        events
    }

    /// Fold a resumed session's completed runs in.
    pub fn restore_workflows(&mut self, restored: Vec<RestoredWorkflowRun>) -> Vec<Event> {
        if !self.workflows.merge_restored(restored) {
            return Vec::new();
        }

        self.workflows
            .snapshot()
            .map(|snapshot| vec![Event::Workflows(snapshot)])
            .unwrap_or_default()
    }

    fn send_control(&mut self, request: Value) -> Result<String, String> {
        let class = match request["subtype"].as_str() {
            Some("get_context_usage") => RequestClass::Query,
            Some("interrupt" | "stop_task") => RequestClass::Control,
            _ => RequestClass::Mutation,
        };

        self.control.check_connected()?;

        let (request_id, message) = self.control.request(request);

        let ticket = self
            .process
            .write_tracked(vec![message])
            .map_err(|error| error.to_string())?;

        self.control
            .record_admitted(request_id.clone(), class, Instant::now());

        self.control.attach_input(&request_id, ticket);

        self.control
            .track(request_id.clone(), PendingControlOperation::Other);

        Ok(request_id)
    }

    fn try_send(&mut self, message: Value) -> Result<(), String> {
        if self.control.is_closed() {
            return Err("Claude is not connected".into());
        }

        self.process
            .write_line(message)
            .map_err(|error| error.to_string())
    }

    /// Write one line; write failures stay unsurfaced because the reader-side
    /// EOF is the single exit-detection path.
    fn send(&mut self, message: Value) {
        let _ = self.process.write_line(message);
    }

    fn on_system(&mut self, message: &Value) -> Vec<Event> {
        match message["subtype"].as_str() {
            Some("init") => self.on_init(message),
            Some("status") => compaction_progress(&mut self.compacting, message),

            Some("compact_boundary") => {
                self.compacting = false;

                self.transcript.on_compact_boundary(message)
            }

            // Every other subtype (hook_*, thinking_tokens, informational, …)
            // is telemetry the UI ignores.
            _ => Vec::new(),
        }
    }

    fn on_init(&mut self, message: &Value) -> Vec<Event> {
        self.control.complete(INIT_REQUEST_ID);

        // The session id makes this conversation resumable by a future tab
        // (`--resume`); captured on every `init` since a resumed session
        // keeps the id of the transcript it reloaded.
        if let Some(session_id) = message["session_id"].as_str() {
            self.session_id = Some(session_id.to_string());
        }

        // `init` — emitted when the first turn opens — carries the session's
        // ACTUAL permission mode, which the initialize response does not
        // (its value is this client's best guess from config). Always
        // applied: any user pick was already sent as a control request
        // before the message that opened this turn, so `init` reports the
        // post-change state and cannot clobber it. The model is only taken
        // before the handshake settled: `init` reports the resolved model id
        // (e.g. `claude-opus-5[1m]`), which is not a catalog value, so
        // adopting it later would break the catalog-driven picker display.
        let model = if self.ready {
            self.applied_model.clone()
        } else {
            message["model"].as_str().map(str::to_owned)
        };

        let permission = message["permissionMode"].as_str().map(str::to_owned);

        self.ready = true;
        self.applied_model = model.clone();
        self.applied_permission = permission.clone();

        let mut events = vec![Event::Ready(ThreadSettings {
            model,
            approval: permission,
            ..ThreadSettings::default()
        })];

        // Older Claude versions only reveal this string catalog when the
        // first turn opens. It must not erase richer initialize metadata.
        if let Some(commands) = legacy_command_catalog(
            self.structured_commands_published,
            &message["slash_commands"],
        ) {
            events.push(Event::Commands(commands));
        }

        // The window grew with whatever this turn loaded, so the breakdown is
        // refreshed here too. A resumed conversation is covered earlier, at the
        // initialize response, because the CLI withholds this message until a
        // model turn actually starts.
        self.request_context_composition();

        events
    }

    fn on_result(&mut self, message: &Value) -> Vec<Event> {
        self.turn_active = false;
        self.turn_reported = false;

        let mut events = self.control.finish_turn();

        // Compaction only runs inside a turn, so a still-set flag here means
        // its end notification was lost (interrupt, aborted turn); the
        // indicator must not outlive the turn that owned it.
        if self.compacting {
            self.compacting = false;
            events.push(Event::CompactionFinished { error: None });
        }

        let error = claude_result_error(message);

        if let Some(name) = self.active_slash_command.take() {
            events.push(Event::SlashCommandResult {
                name,
                outcome: match error.as_ref() {
                    Some(message) => SlashCommandOutcome::Rejected {
                        message: message.clone(),
                    },

                    None => SlashCommandOutcome::Completed { message: None },
                },
            });
        }

        events.extend(self.transcript.finish_turn(message));

        events.push(Event::TurnCompleted {
            error: error.clone(),
        });

        if let Some(id) = self.accepted_identity.take() {
            events.push(Event::ProviderTurnFinished { id, error });
        }

        // The window only changes as the conversation grows, so a settled turn
        // is the point where a fresh breakdown is worth asking for.
        self.request_context_composition();

        events
    }

    fn on_control_request(&mut self, message: &Value) -> Vec<Event> {
        let request_id = message["request_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let request = &message["request"];

        if request["subtype"].as_str() != Some("can_use_tool") {
            // Unsupported server→client requests (hook_callback, mcp_message,
            // …) get an error reply so the turn can't hang on them.
            self.send(json!({
                "type": "control_response",
                "response": {
                    "subtype": "error",
                    "request_id": request_id,
                    "error": "not supported by NiumaTerm agent tab",
                },
            }));

            return Vec::new();
        }

        let tool_name = request["tool_name"].as_str().unwrap_or("tool");

        // AskUserQuestion is a permission request only in shape: the CLI runs
        // the tool with whatever answers the client merges into `updatedInput`,
        // so it needs the question card rather than an allow/deny card.
        if tool_name == "AskUserQuestion" {
            let questions = parse_questions(&request["input"]);

            // Nothing renderable means nothing the user could answer; denying
            // keeps the turn moving instead of showing an empty card.
            if questions.is_empty() {
                self.send(json!({
                    "type": "control_response",
                    "response": {
                        "subtype": "success",
                        "request_id": request_id,
                        "response": {
                            "behavior": "deny",
                            "message": "The question payload was unusable; make your best assumption and continue.",
                        },
                    },
                }));

                return Vec::new();
            }

            self.control.pending_questions = Some(PendingQuestions {
                request_id,
                input: request["input"].clone(),
                questions: questions.clone(),
            });

            return vec![Event::QuestionsRequested { questions }];
        }

        let description = approval_description(tool_name, &request["input"]);

        self.control.pending_approval = Some(PendingApproval {
            request_id,
            input: request["input"].clone(),
            suggestions: (!request["permission_suggestions"].is_null())
                .then(|| request["permission_suggestions"].clone()),
        });

        vec![Event::ApprovalRequested { description }]
    }

    fn on_control_response(&mut self, message: &Value) -> Vec<Event> {
        let response = &message["response"];

        if let Some(id) = response["request_id"].as_str() {
            self.control.complete(id);
        }

        if response["request_id"].as_str() != Some(INIT_REQUEST_ID) {
            let Some(event) = self.control.resolve(response) else {
                return Vec::new();
            };

            if let Event::ContextCompositionUpdated(composition) = &event
                && let Some(usage) = self.transcript.apply_composition(composition)
            {
                return vec![Event::ContextWindowUpdated(usage), event];
            }

            return vec![event];
        }

        if response["subtype"].as_str() == Some("error") {
            let error = response["error"]
                .as_str()
                .unwrap_or("unknown Claude control error")
                .to_string();

            return vec![Event::Error {
                message: error,
                // A failed initialize means the CLI rejected this client.
                fatal: response["request_id"].as_str() == Some(INIT_REQUEST_ID),
            }];
        }

        // The initialize response arrives before any turn and carries the
        // model catalog, so the pickers show real values immediately. It
        // does NOT report the session's current permission mode, and the CLI
        // resolves its startup mode from user config — so the initial value
        // comes from the same config file (`permissions.defaultMode`); the
        // first turn's `init` message then confirms or corrects it. A model
        // resolved at spawn reaches the CLI as `--model`, so it is already
        // applied here; a launch that named none starts on the catalog's
        // "default" entry.
        if response["request_id"].as_str() == Some(INIT_REQUEST_ID) && !self.ready {
            let permission =
                Some(configured_permission_mode().unwrap_or_else(|| "default".to_string()));

            let model = initial_ready_model(self.applied_model.as_deref());

            self.ready = true;
            self.applied_model = Some(model.clone());
            self.applied_permission = permission.clone();

            let mut events = vec![
                Event::Ready(ThreadSettings {
                    model: Some(model.clone()),
                    approval: permission,
                    ..ThreadSettings::default()
                }),
                Event::Models(parse_models(&response["response"]["models"], Some(&model))),
            ];

            if let Some((commands, structured)) = initialize_command_catalog(&response["response"])
            {
                self.structured_commands_published = structured;
                events.push(Event::Commands(commands));
            }

            // This is the only readiness a resumed conversation reaches before
            // the user speaks: the CLI withholds `system/init` until its next
            // model turn. Asking here is what fills the context indicator for
            // a session restored from history.
            self.request_context_composition();

            return events;
        }

        Vec::new()
    }
}

/// Whether a line is model output, which the CLI only emits inside a turn.
/// The turn's `system`/`init` line arrives first but is also emitted on
/// startup and on resume, where no turn has opened yet.
fn carries_model_output(message: &Value) -> bool {
    matches!(message["type"].as_str(), Some("assistant" | "stream_event"))
}
