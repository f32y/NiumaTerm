//! One tab's conversation on the shared host.
//!
//! Unlike the CLI-backed adapters, a session here owns no process: the host
//! outlives every tab, and this holds a session id on it plus the reader
//! threads feeding the pane.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::background_task::{
    BackgroundTaskKey, BackgroundTaskRefs, BackgroundTaskTranscriptUpdate,
};
use crate::chat::{
    Event, SlashCommandArguments, SlashCommandInfo, SlashCommandRunPolicy, SlashCommandSource,
    ThreadSettings,
};
use crate::deepseek::api::ApiClient;
use crate::deepseek::events::Downlinks;
use crate::deepseek::host::{self, Host, HostError};
use crate::deepseek::mapping::{self, ApprovalRequest, QuestionRequest, ToolTracker};
use crate::deepseek::models::ModelDirectory;
use crate::deepseek::projections::ProjectionTracker;
use crate::deepseek::workflows::WorkflowTracker;
use crate::deepseek::{commands, frames, history, presets, subagents};
use crate::workspace::AgentWorkspace;

mod actions;
mod loads;

pub(crate) use crate::deepseek::session::loads::queued_prompts;
pub(super) use crate::deepseek::session::loads::{
    fork_checkpoint_events, history_events, search_events, workflow_transcript_events,
};
use crate::deepseek::session::loads::{
    load_agent_presets, load_commands, load_models, load_sessions, load_skills,
};

pub struct Session {
    client: ApiClient,
    session_id: String,
    /// The project directory this tab works in. Held because it decides which
    /// persisted conversations the tab can continue, and because reattaching to
    /// one requires naming the same directory it was rooted in.
    cwd: Option<String>,
    /// Every open session holds the shared host, which stops when the last one
    /// drops. This is what makes the host outlive individual tabs without
    /// outliving all of them.
    host: Arc<Host>,
    /// Reader threads for both downlinks; dropping them ends the delivery.
    _downlinks: Downlinks,
    /// The pane's delivery channel, for results of unary calls. Whatever a
    /// background read produces has to arrive the same way a pushed frame does,
    /// because that is the only path that wakes the tab.
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
    /// The profile's model and effort, reapplied to a conversation this tab
    /// continues later: the directory belongs to the session, not to the tab.
    model: Option<String>,
    effort: Option<String>,
    /// Whether that model is declared image-capable in the provider's
    /// configured catalog when a conversation starts. Kept for the same reason
    /// the model is: a resumed conversation reads its directory afresh.
    declares_image_input: bool,
    /// The turn state this side knows about, so a stop is only offered while a
    /// turn is actually running.
    running: bool,
    /// Pending prompt identities from the Harness's latest whole-inbox
    /// snapshot. Closing removes them before cancelling the current turn.
    queued_prompt_ids: Vec<String>,
    /// The approval the harness is currently blocked on. Held because the
    /// answer has to carry identities the transcript vocabulary does not.
    pending_approval: Option<ApprovalRequest>,
    /// The question batch the harness is currently blocked on, held for the
    /// same reason: the answer is matched against the asked question ids.
    pending_questions: Option<QuestionRequest>,
    /// Tool calls awaiting their result, so a result can complete the row its
    /// call opened rather than starting a second one.
    tools: ToolTracker,
    /// The usage projections seen so far. Each arrives as its own frame, and
    /// the pane's snapshot is assembled from more than one of them.
    usage: ProjectionTracker,
    /// What the picker offers, and what a pick from it addresses. Empty until
    /// the catalog arrives, which is a background call rather than part of
    /// opening the conversation.
    models: ModelDirectory,
    /// Counter carried by each child-agent catalog read. The catalog is a call
    /// and several can be in flight, so this is what tells a stale answer from
    /// the newest one.
    subagent_activity: u64,
    /// Which of the harness's two child kinds each known child is. Reading a
    /// child's conversation addresses it by that kind, and only the catalog
    /// reports it, so the answer is kept from the catalog that named the child.
    subagent_modes: HashMap<String, bool>,
    /// Workflow runs accumulated from the log. Each event carries only its own
    /// increment, so the run is what they add up to rather than a value any one
    /// of them reports.
    workflows: WorkflowTracker,
}

/// Frame type this adapter mints locally to carry the model catalog.
///
/// The catalog is a unary call, not a downlink push, but the pane only reacts
/// to what arrives on the delivery channel — a result parked anywhere else
/// would sit unread until some unrelated frame happened to wake the tab. The
/// `nmt/` prefix keeps it out of the harness's own type space.
const MODELS_FRAME: &str = "nmt/models";
const HISTORY_FRAME: &str = "nmt/history";
const SEARCH_FRAME: &str = "nmt/search";
const REPLAY_FRAME: &str = "nmt/replay";
const COMMANDS_FRAME: &str = "nmt/commands";
const SUBAGENTS_FRAME: &str = "nmt/subagents";
const SUBAGENT_TRANSCRIPT_FRAME: &str = "nmt/subagent-transcript";
const SKILLS_FRAME: &str = "nmt/skills";
const PRESETS_FRAME: &str = "nmt/agent-presets";
const WORKFLOW_TRANSCRIPT_FRAME: &str = "nmt/workflow-transcript";
const FORK_CHECKPOINTS_FRAME: &str = "nmt/fork-checkpoints";
/// The pending-inbox snapshot is the one frame type the harness itself
/// publishes under its own name rather than through the nmt bridge.
const QUEUE_FRAME: &str = "session/queue";

/// How much of a resumed conversation is rebuilt. The harness pages history at
/// whole-message boundaries, so this is a count of messages rather than of
/// events; one page is what the pane shows, and older turns stay in the log.
const REPLAY_MESSAGES: u64 = 200;

/// How far back the branch-point picker looks. Larger than the replay window
/// because a row costs one line here rather than a rebuilt turn, and a cut is
/// worth offering at prompts that scrolled out of the rebuilt transcript.
const FORK_CHECKPOINT_MESSAGES: u64 = 1000;

/// Combine the independently loaded model directory and permission projection
/// into the settings snapshot that marks the session ready.
pub(crate) fn ready_settings(
    models: &ModelDirectory,
    projections: &ProjectionTracker,
) -> ThreadSettings {
    ThreadSettings {
        model: models.selected().map(str::to_string),
        approval: projections.permission().map(str::to_string),
        effort: models.effort().map(str::to_string),
        ..ThreadSettings::default()
    }
}

/// A conversation this tab has just opened or reattached to.
struct OpenedConversation {
    session_id: String,
    /// The composition it was built from, absent when the deployment composes
    /// no presets at all and every conversation shares the host's own.
    agent_preset: Option<String>,
}

/// Open a conversation on the host, or reattach to an existing one.
///
/// The same call serves both: naming an existing id returns that session
/// unchanged when the directory matches, and refuses when it does not, which is
/// what makes reattaching safe to attempt without a separate probe.
/// What NiumaTerm asks the harness to open a conversation with.
///
/// The header carries exactly one working directory, which the harness
/// resolves into the single workspace root of the session's sandbox. Additional
/// workspace directories therefore have no field to travel in and are
/// deliberately absent rather than approximated: no common ancestor is
/// substituted, and no broader permission preset is selected on the user's
/// behalf. The Agent Tab discloses what that leaves out.
pub(crate) fn session_create_payload(cwd: Option<&str>, session_id: Option<&str>) -> Value {
    let mut payload = json!({});
    if let Some(cwd) = cwd {
        payload["cwd"] = json!(cwd);
    }
    if let Some(session_id) = session_id {
        payload["sessionId"] = json!(session_id);
    }
    payload
}

fn open_conversation(
    client: &ApiClient,
    cwd: Option<&str>,
    session_id: Option<&str>,
) -> Result<OpenedConversation, String> {
    let created = client
        .request("session/create", session_create_payload(cwd, session_id))
        .map_err(|error| error.message().to_string())?;

    let session_id = created["sessionId"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "the harness answered without a conversation id".to_string())?;

    Ok(OpenedConversation {
        session_id,
        agent_preset: created["agentPreset"].as_str().map(str::to_string),
    })
}

impl Session {
    /// Commands this adapter serves itself, beside the ones the harness's own
    /// registry reports.
    ///
    /// Each one addresses a session-management method rather than the command
    /// registry, so none of them can arrive through discovery; the harness
    /// serves them to its own browser UI as ordinary buttons, which this
    /// composer has no equivalent of.
    pub fn adapter_commands() -> Vec<SlashCommandInfo> {
        vec![
            SlashCommandInfo {
                name: "rename".to_string(),
                description: "Pin a title on this conversation".to_string(),
                argument_hint: Some("<title>".to_string()),
                source: SlashCommandSource::Adapter,
                arguments: SlashCommandArguments::Freeform,
                run_policy: SlashCommandRunPolicy::Immediate,
            },
            SlashCommandInfo {
                name: "fork".to_string(),
                description: "Branch this conversation in front of an earlier prompt".to_string(),
                argument_hint: None,
                source: SlashCommandSource::Adapter,
                arguments: SlashCommandArguments::None,
                // The harness cuts a branch on whole turns and refuses one
                // anchored inside a turn still running, so a branch asked for
                // mid-turn has no cut to offer.
                run_policy: SlashCommandRunPolicy::IdleOnly,
            },
            SlashCommandInfo {
                name: "find".to_string(),
                description: "Search earlier conversations for a phrase".to_string(),
                argument_hint: Some("<text>".to_string()),
                source: SlashCommandSource::Adapter,
                arguments: SlashCommandArguments::Freeform,
                run_policy: SlashCommandRunPolicy::Immediate,
            },
        ]
    }

    /// Create a conversation on the running host and start delivering its
    /// frames. `cwd` is the project directory this tab works in; the host's own
    /// working directory applies when it is absent.
    pub fn create(
        launch: &crate::LaunchConfig,
        workspace: &AgentWorkspace,
        deliver: impl Fn(Value) + Send + Sync + 'static,
    ) -> Result<Self, HostError> {
        // The installed Harness resolves exactly one workspace root from the
        // session header, and its workspace-write policy has no additional
        // writable roots. Sending only the primary directory is therefore the
        // truthful reduction; approximating the rest with a common ancestor
        // would hand the agent directories the user never selected. The Agent
        // Tab discloses what is missing before the first prompt.
        let cwd = workspace.primary().map(str::to_string);
        // The host is shared by every DeepSeek tab and keyed by launch
        // configuration alone, so which directories a conversation uses must
        // not enter that identity.
        let host = host::shared(launch)?;
        let client = host.client().clone();
        let opened = open_conversation(&client, cwd.as_deref(), None).map_err(|error| {
            HostError::FailedToStart(format!(
                "the harness could not open a conversation: {error}"
            ))
        })?;
        let session_id = opened.session_id;

        // Opening the downlinks after the session exists means its first frames
        // cannot be missed: the stream replays a baseline for every attached
        // session when it opens.
        let deliver: Arc<dyn Fn(Value) + Send + Sync> = Arc::new(deliver);
        let (downlinks, snapshot) = Downlinks::open(
            client.clone(),
            Arc::downgrade(&host),
            session_id.clone(),
            Arc::clone(&deliver),
        )
        .map_err(HostError::FailedToStart)?;

        load_models(
            client.clone(),
            session_id.clone(),
            snapshot["projections"]["values"]["modelSelection"]["next"].clone(),
            launch.model.clone(),
            launch.effort.clone(),
            launch.declares_image_input,
            Arc::clone(&deliver),
        );
        // The list is read now rather than when the picker opens, because the
        // picker refuses to open on an empty list and cannot wait for one.
        load_sessions(client.clone(), cwd.clone(), Arc::clone(&deliver));
        load_commands(client.clone(), session_id.clone(), Arc::clone(&deliver));
        load_skills(client.clone(), session_id.clone(), Arc::clone(&deliver));
        load_agent_presets(
            client.clone(),
            session_id.clone(),
            opened.agent_preset,
            Arc::clone(&deliver),
        );

        Ok(Self {
            client,
            session_id,
            cwd,
            host,
            _downlinks: downlinks,
            deliver,
            model: launch.model.clone(),
            effort: launch.effort.clone(),
            declares_image_input: launch.declares_image_input,
            running: false,
            queued_prompt_ids: Vec::new(),
            pending_approval: None,
            pending_questions: None,
            tools: ToolTracker::default(),
            usage: ProjectionTracker::default(),
            models: ModelDirectory::default(),
            subagent_activity: 0,
            subagent_modes: HashMap::new(),
            workflows: WorkflowTracker::default(),
        })
    }

    /// Continue an earlier conversation in place.
    ///
    /// The tab keeps its host and replaces its session-specific subscriptions.
    /// The new streams must open successfully before releasing the old ones,
    /// so a rejected resume leaves the current conversation usable.
    pub fn resume_thread(&mut self, thread_id: &str) -> bool {
        match open_conversation(&self.client, self.cwd.as_deref(), Some(thread_id)) {
            Ok(opened) => {
                let (downlinks, snapshot) = match Downlinks::open(
                    self.client.clone(),
                    Arc::downgrade(&self.host),
                    opened.session_id.clone(),
                    Arc::clone(&self.deliver),
                ) {
                    Ok(opened) => opened,
                    Err(message) => {
                        tracing::warn!("deepseek could not follow {thread_id}: {message}");
                        return false;
                    }
                };
                self.session_id = opened.session_id;
                self._downlinks = downlinks;
                // Everything below describes the conversation this tab just
                // left; carrying it over would attribute it to the new one.
                self.running = false;
                self.queued_prompt_ids.clear();
                self.pending_approval = None;
                self.pending_questions = None;
                self.tools = ToolTracker::default();
                self.usage = ProjectionTracker::default();
                self.models = ModelDirectory::default();
                self.subagent_activity = 0;
                self.subagent_modes.clear();
                self.workflows = WorkflowTracker::default();

                // The directory belongs to the session, so the resumed one is
                // asked afresh and the profile's pick applied to it in turn.
                load_models(
                    self.client.clone(),
                    self.session_id.clone(),
                    snapshot["projections"]["values"]["modelSelection"]["next"].clone(),
                    self.model.clone(),
                    self.effort.clone(),
                    self.declares_image_input,
                    Arc::clone(&self.deliver),
                );
                load_sessions(
                    self.client.clone(),
                    self.cwd.clone(),
                    Arc::clone(&self.deliver),
                );
                // Commands and skills are scoped to the agent and its project,
                // and a resumed conversation may have been composed from a
                // different preset or rooted elsewhere.
                load_commands(
                    self.client.clone(),
                    self.session_id.clone(),
                    Arc::clone(&self.deliver),
                );
                load_skills(
                    self.client.clone(),
                    self.session_id.clone(),
                    Arc::clone(&self.deliver),
                );
                load_agent_presets(
                    self.client.clone(),
                    self.session_id.clone(),
                    opened.agent_preset,
                    Arc::clone(&self.deliver),
                );
                true
            }
            Err(error) => {
                tracing::warn!("deepseek could not continue {thread_id}: {error}");
                false
            }
        }
    }

    /// Whether the shared host is still serving. A host that exited takes every
    /// tab's session with it, so a tab reports that rather than failing each
    /// later action on its own.
    pub fn host_is_running(&self) -> bool {
        self.host.is_running()
    }

    /// Map one delivered frame into transcript events. Frames for other
    /// sessions and types this build does not know produce nothing.
    pub fn process(&mut self, frame: Value) -> Vec<Event> {
        // An approval is answerable, so recognizing it means recording what an
        // answer will need. The stream replays a still-pending request when it
        // reconnects, and re-raising the card from the replay is what lets a
        // tab that lost its socket mid-question still be answered.
        if let Some(request) = mapping::approval_request(&frame, &self.session_id) {
            let description = request.description.clone();
            self.pending_approval = Some(request);
            return vec![Event::ApprovalRequested { description }];
        }

        // Questions replay on reconnect exactly as approvals do, so the same
        // rule applies: recognizing the frame is what makes it answerable.
        if let Some((request, questions)) = mapping::question_request(&frame, &self.session_id) {
            self.pending_questions = Some(request);
            return vec![Event::QuestionsRequested { questions }];
        }

        // A projection frame carries one unit's whole value, and the snapshots
        // the pane renders are folded from several of them, so this is the one
        // mapping that has to remember what the earlier frames said.
        if let Some(events) = self.usage.apply(&frame, &self.session_id) {
            return events;
        }

        let payload = &frame["payload"];
        match payload["type"].as_str() {
            Some("nmt/connection-reset") if self.is_current_session(payload) => {
                self.pending_approval = None;
                self.pending_questions = None;
                return vec![Event::ApprovalResolved, Event::QuestionsResolved];
            }
            Some(SUBAGENTS_FRAME) => return self.on_subagents(payload),
            Some(SUBAGENT_TRANSCRIPT_FRAME) => return self.on_subagent_transcript(payload),
            Some(WORKFLOW_TRANSCRIPT_FRAME) => return workflow_transcript_events(payload),
            Some(SKILLS_FRAME) => return self.on_skills(payload),
            Some(PRESETS_FRAME) => return self.on_presets(payload),
            Some(COMMANDS_FRAME) => return self.on_commands(payload),
            Some(HISTORY_FRAME) => return history_events(payload),
            Some(SEARCH_FRAME) => return search_events(payload),
            // A queue snapshot for a conversation this tab has since left is
            // not this tab's inbox, but the frame still carries ordinary log
            // events, so it falls through to the mapping below instead of
            // being swallowed here.
            Some(QUEUE_FRAME) if self.is_current_session(payload) => {
                return self.on_queue(payload);
            }
            Some(REPLAY_FRAME) => return self.on_replay(payload),
            Some(FORK_CHECKPOINTS_FRAME) => return fork_checkpoint_events(payload),
            Some(MODELS_FRAME) => return self.on_models(payload),
            _ => {}
        }

        // A child announces itself in the parent's own log, and the catalog is
        // a call the pane would otherwise have no reason to make: the panel
        // that would ask for one is hidden until a child is known to exist.
        // A finished turn re-reads it because a child's activity is sampled
        // when asked rather than pushed.
        let event_type = payload["event"]["type"].as_str();
        if self.is_current_session(payload)
            && (event_type == Some("subagent/descriptor")
                || (event_type == Some("turn/end") && !self.subagent_modes.is_empty()))
        {
            self.refresh_background_tasks();
        }

        let mut events = mapping::map_frame(&frame, &self.session_id, &mut self.tools);

        // Workflow rows are folded from the log rather than mapped one event to
        // one row, so they are published beside whatever else the frame
        // produced instead of through the transcript vocabulary.
        if self.is_current_session(payload) && self.workflows.apply(&payload["event"]) {
            events.push(Event::Workflows(self.workflows.snapshot(&self.session_id)));
        }

        for event in &events {
            match event {
                Event::TurnStarted => self.running = true,
                Event::TurnCompleted { .. } => {
                    self.running = false;
                    // A turn that ended cannot still be waiting on an answer.
                    self.pending_approval = None;
                    self.pending_questions = None;
                }
                Event::ApprovalResolved => self.pending_approval = None,
                Event::QuestionsResolved => self.pending_questions = None,
                _ => {}
            }
        }

        events
    }

    /// Whether a frame names the conversation this tab holds. A read for a
    /// conversation the tab has since left describes an agent it no longer
    /// talks to.
    fn is_current_session(&self, payload: &Value) -> bool {
        payload["sessionId"].as_str() == Some(&self.session_id)
    }

    fn on_subagents(&mut self, payload: &Value) -> Vec<Event> {
        let Some(frame) = frames::parse::<frames::SubagentsFrame>(SUBAGENTS_FRAME, payload) else {
            return Vec::new();
        };
        // Several reads can be in flight, and an older answer describes a
        // moment the panel has already moved past.
        if frame.session_id != self.session_id || frame.activity < self.subagent_activity {
            return Vec::new();
        }
        let snapshot = subagents::snapshot(&frame.catalog, &self.session_id, frame.activity);
        self.subagent_modes = snapshot
            .tasks
            .iter()
            .filter_map(|task| match &task.refs {
                BackgroundTaskRefs::DeepSeek { continuable, .. } => {
                    Some((task.key.id.clone(), *continuable))
                }
                _ => None,
            })
            .collect();
        vec![Event::BackgroundTasks(snapshot)]
    }

    fn on_subagent_transcript(&self, payload: &Value) -> Vec<Event> {
        let Some(frame) =
            frames::parse::<frames::SubagentTranscriptFrame>(SUBAGENT_TRANSCRIPT_FRAME, payload)
        else {
            return Vec::new();
        };
        if frame.session_id != self.session_id {
            return Vec::new();
        }
        vec![Event::BackgroundTaskTranscript {
            key: BackgroundTaskKey::deepseek(&frame.child_session_id),
            update: BackgroundTaskTranscriptUpdate::loaded(history::items(&frame.page)),
        }]
    }

    fn on_skills(&self, payload: &Value) -> Vec<Event> {
        let Some(frame) = frames::parse::<frames::SkillsFrame>(SKILLS_FRAME, payload) else {
            return Vec::new();
        };
        if frame.session_id != self.session_id {
            return Vec::new();
        }
        vec![Event::Skills(commands::skills(&frame.skills))]
    }

    fn on_presets(&self, payload: &Value) -> Vec<Event> {
        let Some(frame) = frames::parse::<frames::PresetsFrame>(PRESETS_FRAME, payload) else {
            return Vec::new();
        };
        if frame.session_id != self.session_id {
            return Vec::new();
        }
        vec![Event::AgentPresets {
            presets: presets::catalog(&frame.presets),
            current: frame.current,
        }]
    }

    fn on_commands(&self, payload: &Value) -> Vec<Event> {
        let Some(frame) = frames::parse::<frames::CommandsFrame>(COMMANDS_FRAME, payload) else {
            return Vec::new();
        };
        if frame.session_id != self.session_id {
            return Vec::new();
        }
        vec![Event::Commands(commands::catalog(&frame.commands))]
    }

    /// The harness republishes its whole pending inbox after every change,
    /// so this replaces what the tab holds rather than amending it: an
    /// increment would have to guess at removals another client made.
    fn on_queue(&mut self, payload: &Value) -> Vec<Event> {
        let Some(frame) = frames::parse::<frames::QueueFrame>(QUEUE_FRAME, payload) else {
            return Vec::new();
        };
        let prompts = queued_prompts(&frame.items);
        self.queued_prompt_ids = prompts
            .iter()
            .filter_map(|prompt| prompt.id.clone())
            .collect();
        vec![Event::QueuedPrompts(prompts)]
    }

    fn on_replay(&mut self, payload: &Value) -> Vec<Event> {
        let Some(frame) = frames::parse::<frames::ReplayFrame>(REPLAY_FRAME, payload) else {
            return Vec::new();
        };
        // A page belonging to the conversation this tab has since left
        // would replace the visible transcript with another one's.
        if frame.session_id != self.session_id {
            return Vec::new();
        }
        if let Some(message) = frame.error {
            return vec![Event::Error {
                message,
                // The conversation was attached before the page was read,
                // so the tab works; only its earlier turns are missing.
                fatal: false,
            }];
        }

        let page = &frame.page;
        // The tail page is also where a projection's current value can be
        // read: a live push reports only what changed after the tab
        // attached, so accounting and the permission preset would otherwise
        // stay blank until one of them happened to move.
        let mut events = self.usage.apply_baseline(
            &page["projections"]["values"],
            page["projections"]["asOfSeq"].as_i64(),
        );

        // A run's rows are folded from the same events the log carries, so
        // a resumed conversation rebuilds them from its own history rather
        // than from a record kept beside it.
        let mut folded = false;
        self.tools = ToolTracker::default();
        self.workflows = WorkflowTracker::default();
        self.running = false;
        for entry in page["records"].as_array().into_iter().flatten() {
            folded |= self.workflows.apply(&entry["event"]);
            for event in mapping::map_session_event(&entry["event"], &Value::Null, &mut self.tools)
            {
                match event {
                    Event::TurnStarted => self.running = true,
                    Event::TurnCompleted { .. } => self.running = false,
                    _ => {}
                }
            }
        }
        if folded {
            events.push(Event::Workflows(self.workflows.snapshot(&self.session_id)));
        }

        events.push(Event::Replay(history::replay(page)));
        if self.running {
            events.push(Event::TurnStarted);
        }
        events
    }

    fn on_models(&mut self, payload: &Value) -> Vec<Event> {
        let Some(frame) = frames::parse::<frames::ModelsFrame>(MODELS_FRAME, payload) else {
            return Vec::new();
        };
        if frame.session_id != self.session_id {
            return Vec::new();
        }
        self.models = ModelDirectory::parse(&frame.models);
        // The catalog and the selection travel together, so the pickers
        // gain their options and their current value in one repaint.
        let mut events = vec![
            Event::Models(self.models.catalog()),
            Event::Ready(ready_settings(&self.models, &self.usage)),
        ];

        // A refused selection travels with the catalog that outlived it, so
        // the level reported alongside the reason is the one the session is
        // actually on rather than the one that was asked for.
        if let Some(message) = frame.error {
            events.push(Event::EffortRejected {
                message,
                effort: self.models.effort().map(str::to_string),
            });
        }

        events
    }
}
