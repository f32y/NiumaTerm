//! One tab's conversation on the shared host.
//!
//! Unlike the CLI-backed adapters, a session here owns no process: the host
//! outlives every tab, and this holds a session id on it plus the reader
//! threads feeding the pane.

pub(crate) use crate::dsh::session::loads::queued_prompts;

pub(super) use crate::dsh::session::actions::CloseAction;
#[cfg(test)]
pub(super) use crate::dsh::session::actions::run_close_actions;
#[cfg(test)]
pub(super) use crate::dsh::session::loads::models_with_image;
pub(super) use crate::dsh::session::loads::{
    fork_checkpoint_events, history_events, search_events, workflow_transcript_events,
};

mod actions;
mod controls;
mod lane;
mod loads;
mod switch;

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::background_task::{
    BackgroundTaskKey, BackgroundTaskLoadState, BackgroundTaskRefs, BackgroundTaskSnapshot,
    BackgroundTaskSummary, BackgroundTaskTranscriptUpdate,
};
use crate::chat::{
    Event, ForkAnchor, Item, MessageImage, Question, QuestionMode,
    QuestionRequest as ChatQuestionRequest, QuestionResolution, QuestionResponse, QueuedPrompt,
    SendOutcome, SlashCommandArguments, SlashCommandInfo, SlashCommandOutcome,
    SlashCommandRunPolicy, SlashCommandSource, ThreadSettings,
};
use crate::dsh::api::ApiClient;
use crate::dsh::events::Downlinks;
use crate::dsh::host::{self, Host, HostError};
use crate::dsh::mapping::{self, ApprovalRequest, QuestionRequest, ToolTracker};
use crate::dsh::models::ModelDirectory;
use crate::dsh::projections::ProjectionTracker;
use crate::dsh::session::actions::{prompt_payload, run_slash, schedule_close_actions};
use crate::dsh::session::controls::{COMPLETED_FRAME, Controls, Operation, question_id};
use crate::dsh::session::lane::CommandLane;
use crate::dsh::session::loads::{
    ModelProfile, check_running, failed_read_events, load_agent_presets, load_commands,
    load_conversation, load_fork_checkpoints, load_search, load_sessions, load_skills,
    load_subagent_transcript, load_subagents, load_workflow_transcript,
};
use crate::dsh::session::switch::{Switch, SwitchSlot, Switching, Target, switch_conversation};
use crate::dsh::workflows::WorkflowTracker;
use crate::dsh::{catalogs, frames, history};
use crate::workspace::AgentWorkspace;

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
    downlinks: Downlinks,

    /// The pane's delivery channel, for results of unary calls. Whatever a
    /// background read produces has to arrive the same way a pushed frame does,
    /// because that is the only path that wakes the tab.
    deliver: Arc<dyn Fn(Value) + Send + Sync>,

    /// The profile's model pick, reapplied to a conversation this tab
    /// continues later: the directory belongs to the session, not to the tab.
    profile: ModelProfile,

    /// The turn state this side knows about, so a stop is only offered while a
    /// turn is actually running.
    running: bool,

    /// The Harness's latest whole-inbox snapshot. Closing removes its entries
    /// before cancelling the current turn, and a refused change to the inbox
    /// republishes it so the composer shows what is still pending.
    queued_prompts: Vec<QueuedPrompt>,

    /// The composition this conversation was built from, as the preset
    /// catalog last reported it. A refused recomposition reports it again so
    /// the picker returns to it.
    agent_preset: Option<String>,

    /// Commands issued on this conversation's behalf, answered by frames.
    lane: CommandLane,

    /// Where a conversation change leaves the streams it opened.
    switch: SwitchSlot,

    /// The approval the harness is currently blocked on. Held because the
    /// answer has to carry identities the transcript vocabulary does not.
    pending_approval: Option<ApprovalRequest>,

    /// The question batch the harness is currently blocked on, held for the
    /// same reason: the answer is matched against the asked question ids.
    pending_questions: Option<QuestionRequest>,

    controls: Controls,

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

    /// Rows from the newest child catalog and from the newest job list. The
    /// panel takes one whole snapshot per conversation while the two sources
    /// update independently, so each keeps the other's last answer.
    subagent_rows: Vec<BackgroundTaskSummary>,

    job_rows: Vec<BackgroundTaskSummary>,

    /// Counts job list changes. Added to the catalog counter so the snapshot
    /// ordinal still advances when only a job starts or settles.
    job_activity: u64,

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

/// Whether the harness is running a turn for this session, as its session
/// list and status events report it.
pub(crate) const SESSION_STATUS: &str = "host/session-status";

const SEARCH_FRAME: &str = "nmt/search";
const REPLAY_FRAME: &str = "nmt/replay";
const COMMANDS_FRAME: &str = "nmt/commands";
const SUBAGENTS_FRAME: &str = "nmt/subagents";
const SUBAGENT_TRANSCRIPT_FRAME: &str = "nmt/subagent-transcript";
const SKILLS_FRAME: &str = "nmt/skills";
const PRESETS_FRAME: &str = "nmt/agent-presets";
const WORKFLOW_TRANSCRIPT_FRAME: &str = "nmt/workflow-transcript";
const FORK_CHECKPOINTS_FRAME: &str = "nmt/fork-checkpoints";
const SETTLED_FRAME: &str = "nmt/command-settled";

/// The pending-inbox snapshot and the job list are the frame types the
/// harness itself publishes under its own names rather than through the nmt
/// bridge.
const QUEUE_FRAME: &str = "session/queue";

/// One conversation's background jobs, relayed from the control stream.
const JOBS_FRAME: &str = "session/jobs";

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
pub(super) struct OpenedConversation {
    pub(super) session_id: String,

    /// The composition it was built from, absent when the deployment composes
    /// no presets at all and every conversation shares the host's own.
    pub(super) agent_preset: Option<String>,
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
///
/// A preset is only named for a new conversation: reattaching to one composed
/// from another preset is refused, and the composition it already has is the
/// one its history was produced under.
pub(crate) fn session_create_payload(
    cwd: Option<&str>,
    session_id: Option<&str>,
    agent_preset: Option<&str>,
) -> Value {
    let mut payload = json!({});

    if let Some(cwd) = cwd {
        payload["cwd"] = json!(cwd);
    }

    if let Some(session_id) = session_id {
        payload["sessionId"] = json!(session_id);
    }

    if let Some(agent_preset) = agent_preset {
        payload["agentPreset"] = json!(agent_preset);
    }

    payload
}

/// The frame answering a command issued for `session_id`.
fn settled(session_id: &str, command: Value) -> Value {
    json!({ "payload": {
        "type": SETTLED_FRAME, "sessionId": session_id, "command": command,
    } })
}

async fn open_conversation(
    client: &ApiClient,
    cwd: Option<&str>,
    session_id: Option<&str>,
    agent_preset: Option<&str>,
) -> Result<OpenedConversation, String> {
    let created = client
        .request(
            "session/create",
            session_create_payload(cwd, session_id, agent_preset),
        )
        .await
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

/// Open a new conversation composed from `agent_preset`, and the reason the
/// preset could not be used when the conversation fell back to the default.
///
/// The preset is a remembered pick, and the deployment can since have removed
/// it or lost the ability to compose it. Failing the tab for that would leave
/// every new conversation of the profile unable to start, so a refused preset
/// is retried once on the deployment's default composition.
pub(super) async fn open_new_conversation(
    client: &ApiClient,
    cwd: Option<&str>,
    agent_preset: Option<&str>,
) -> Result<(OpenedConversation, Option<String>), String> {
    match (
        open_conversation(client, cwd, None, agent_preset).await,
        agent_preset,
    ) {
        (Err(error), Some(preset)) => {
            let opened = open_conversation(client, cwd, None, None).await?;

            let refusal = format!(
                "The agent preset \"{preset}\" could not be used, so this conversation \
                 runs on the default one: {error}"
            );

            Ok((opened, Some(refusal)))
        }
        (opened, _) => opened.map(|opened| (opened, None)),
    }
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
    pub async fn create(
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
        let host = host::shared(launch).await?;
        let client = host.client().clone();

        let deliver: Arc<dyn Fn(Value) + Send + Sync> = Arc::new(deliver);

        let opening = async {
            let (opened, preset_refusal) =
                open_new_conversation(&client, cwd.as_deref(), launch.agent_preset.as_deref())
                    .await
                    .map_err(|error| {
                        format!("the harness could not open a conversation: {error}")
                    })?;

            // Opening the downlinks after the session exists means its first
            // frames cannot be missed: the stream replays a baseline for every
            // attached session when it opens.
            let (downlinks, snapshot) = Downlinks::open(
                client.clone(),
                Arc::downgrade(&host),
                opened.session_id.clone(),
                Arc::clone(&deliver),
            )
            .await?;

            Ok((opened, preset_refusal, downlinks, snapshot))
        };

        let (opened, preset_refusal, downlinks, snapshot) =
            opening.await.map_err(HostError::FailedToStart)?;

        let session_id = opened.session_id.clone();

        let profile = ModelProfile {
            model: launch.model.clone(),
            effort: launch.effort.clone(),
            declares_image_input: launch.declares_image_input,
        };

        load_conversation(
            &client,
            &opened,
            preset_refusal,
            cwd.clone(),
            &snapshot,
            &profile,
            &deliver,
        );

        Ok(Self {
            controls: Controls::new(client.clone(), Arc::clone(&deliver)),
            client,
            session_id,
            cwd,
            host,
            downlinks,
            deliver,
            profile,
            running: false,
            queued_prompts: Vec::new(),
            agent_preset: None,
            lane: CommandLane::new(),
            switch: SwitchSlot::default(),
            pending_approval: None,
            pending_questions: None,
            tools: ToolTracker::default(),
            usage: ProjectionTracker::default(),
            models: ModelDirectory::default(),
            subagent_activity: 0,
            subagent_modes: HashMap::new(),
            subagent_rows: Vec::new(),
            job_rows: Vec::new(),
            job_activity: 0,
            workflows: WorkflowTracker::default(),
        })
    }

    /// Continue an earlier conversation in place.
    ///
    /// Answers whether the change was requested. It completes when the opened
    /// streams are taken over in [`Self::process`], and a refusal arrives there
    /// as an error against the conversation the tab is still on.
    pub fn resume_thread(&mut self, thread_id: &str) -> bool {
        self.switch_to(Target::Existing(thread_id.to_string()));

        true
    }

    fn switch_to(&mut self, target: Target) {
        self.lane.run(switch_conversation(
            Switching {
                client: self.client.clone(),
                host: Arc::downgrade(&self.host),
                cwd: self.cwd.clone(),
                current: self.session_id.clone(),
                deliver: Arc::clone(&self.deliver),
                slot: Arc::clone(&self.switch),
            },
            target,
        ));
    }

    /// Take over the conversation a finished change left in the slot. The tab
    /// keeps its host and replaces its session-specific subscriptions.
    fn on_switched(&mut self) {
        // An empty slot means a later change already replaced this one's
        // streams, and its own announcement is still to come.
        let Some(Switch {
            opened,
            downlinks,
            snapshot,
        }) = self.switch.lock().take()
        else {
            return;
        };

        self.session_id = opened.session_id.clone();

        self.controls.clear();

        self.downlinks = downlinks;

        // Everything below describes the conversation this tab just
        // left; carrying it over would attribute it to the new one.
        self.running = false;

        self.queued_prompts.clear();

        self.agent_preset = None;
        self.pending_approval = None;
        self.pending_questions = None;
        self.tools = ToolTracker::default();
        self.usage = ProjectionTracker::default();
        self.models = ModelDirectory::default();
        self.subagent_activity = 0;

        self.subagent_modes.clear();
        self.subagent_rows.clear();
        self.job_rows.clear();

        self.job_activity = 0;

        self.workflows = WorkflowTracker::default();

        load_conversation(
            &self.client,
            &opened,
            None,
            self.cwd.clone(),
            &snapshot,
            &self.profile,
            &self.deliver,
        );
    }

    /// Map one delivered frame into transcript events. Frames for other
    /// sessions and types this build does not know produce nothing.
    pub fn process(&mut self, frame: Value) -> Vec<Event> {
        if let Some(events) = failed_read_events(&frame["payload"], &self.session_id) {
            return events;
        }

        if frame["payload"]["type"] == COMPLETED_FRAME {
            return self.control_completed(&frame["payload"]);
        }

        if frame["payload"]["type"] == SETTLED_FRAME {
            return self.on_settled(&frame["payload"]);
        }

        // An approval is answerable, so recognizing it means recording what an
        // answer will need. The stream replays a still-pending request when it
        // reconnects, and re-raising the card from the replay is what lets a
        // tab that lost its socket mid-question still be answered.
        if let Some(request) = mapping::approval_request(&frame, &self.session_id) {
            return self.on_approval_request(request);
        }

        // Questions replay on reconnect exactly as approvals do, so the same
        // rule applies: recognizing the frame is what makes it answerable.
        if let Some((request, questions)) = mapping::question_request(&frame, &self.session_id) {
            return self.on_question_request(request, questions);
        }

        // A projection frame carries one unit's whole value, and the snapshots
        // the pane renders are folded from several of them, so this is the one
        // mapping that has to remember what the earlier frames said.
        if let Some(events) = self.usage.apply(&frame, &self.session_id) {
            return events;
        }

        let payload = &frame["payload"];

        let resolved_identity = match payload["type"].as_str() {
            Some("approval/resolved") => self
                .pending_approval
                .as_ref()
                .map(|request| (&request.client_id, &request.event_id)),
            Some("question/resolved") => self
                .pending_questions
                .as_ref()
                .map(|request| (&request.client_id, &request.event_id)),
            _ => None,
        };

        if let Some((client_id, event_id)) = resolved_identity
            && (frame["eventId"].as_str().is_some_and(|id| id != event_id)
                || frame["clientId"].as_str().is_some_and(|id| id != client_id))
        {
            return Vec::new();
        }

        match payload["type"].as_str() {
            Some("question/resolved") if self.is_current_session(payload) => {
                return self.expire_questions();
            }
            Some("nmt/connection-reset") if self.is_current_session(payload) => {
                return self.on_connection_reset();
            }
            Some("nmt/host-exited") if self.is_current_session(payload) => {
                return self.on_host_exited();
            }
            Some(SUBAGENTS_FRAME) => return self.on_subagents(payload),
            Some(JOBS_FRAME) => return self.on_jobs(payload),
            Some(SUBAGENT_TRANSCRIPT_FRAME) => return self.on_subagent_transcript(payload),
            Some(WORKFLOW_TRANSCRIPT_FRAME) => {
                return workflow_transcript_events(payload, &self.session_id);
            }
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
            Some(FORK_CHECKPOINTS_FRAME) => {
                return fork_checkpoint_events(payload, &self.session_id);
            }
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

        // The harness raises an agent error only on its way out of the turn
        // driver, so the agent is idle once one is reported. When the failure
        // is the turn-end record itself being rejected, the log never closes
        // the turn and this frame is the only sign it stopped. The error and
        // the log travel on separate streams, so a log close that arrives
        // after the error is dropped: completing twice would settle whatever
        // turn the tab starts next.
        match payload["type"].as_str() {
            Some("host/agent-error") if self.running && self.is_current_session(payload) => {
                events.push(Event::TurnCompleted { error: None });
            }
            Some(SESSION_STATUS)
                if self.running
                    && payload["running"] == false
                    && self.is_current_session(payload) =>
            {
                events.push(Event::TurnCompleted { error: None });
            }
            _ if !self.running => {
                events.retain(|event| !matches!(event, Event::TurnCompleted { .. }));
            }
            _ => {}
        }

        // Workflow rows are folded from the log rather than mapped one event to
        // one row, so they are published beside whatever else the frame
        // produced instead of through the transcript vocabulary.
        if self.is_current_session(payload) && self.workflows.apply(&payload["event"]) {
            events.push(Event::Workflows(self.workflows.snapshot(&self.session_id)));
        }

        let mut resolved = Vec::new();

        for event in &events {
            match event {
                Event::TurnStarted => self.running = true,
                Event::TurnCompleted { .. } => {
                    self.running = false;

                    self.controls.retire_interrupt();

                    // A turn that ended cannot still be waiting on an answer.
                    self.pending_approval = None;

                    self.controls.retire_approval();

                    resolved.extend(self.expire_questions());
                }
                Event::ApprovalResolved => {
                    self.pending_approval = None;
                }
                _ => {}
            }
        }

        events.extend(resolved);

        events
    }

    fn on_approval_request(&mut self, request: ApprovalRequest) -> Vec<Event> {
        if self.pending_approval.as_ref() != Some(&request) {
            self.controls.retire_approval();
        }

        let description = request.description.clone();

        self.pending_approval = Some(request);

        vec![Event::ApprovalRequested { description }]
    }

    fn on_question_request(
        &mut self,
        request: QuestionRequest,
        questions: Vec<Question>,
    ) -> Vec<Event> {
        let mut events = Vec::new();

        if self
            .pending_questions
            .as_ref()
            .is_some_and(|old| old != &request)
        {
            events.extend(self.expire_questions());
        }

        events.push(Event::InputRequested(ChatQuestionRequest {
            id: question_id(&request),
            mode: QuestionMode::Blocking,
            questions,
        }));

        self.pending_questions = Some(request);

        events
    }

    fn on_connection_reset(&mut self) -> Vec<Event> {
        self.controls.clear();

        self.pending_approval = None;

        let mut events = self.expire_questions();

        events.push(Event::ApprovalResolved);

        events
    }

    /// The host took this conversation with it, so no turn is running and the
    /// tab is told to replace the host instead of sending into a closed port.
    fn on_host_exited(&mut self) -> Vec<Event> {
        self.running = false;

        let mut events = self.on_connection_reset();

        events.push(Event::HostExited {
            message: "DeepSeek Harness host stopped unexpectedly".to_string(),
        });

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

        let snapshot =
            catalogs::subagent_snapshot(&frame.catalog, &self.session_id, frame.activity);

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

        self.subagent_rows = snapshot.tasks;

        vec![self.background_tasks()]
    }

    fn on_jobs(&mut self, payload: &Value) -> Vec<Event> {
        let Some(frame) = frames::parse::<frames::JobsFrame>(JOBS_FRAME, payload) else {
            return Vec::new();
        };

        if frame.session_id != self.session_id {
            return Vec::new();
        }

        self.job_activity += 1;

        self.job_rows = catalogs::job_rows(
            &frame.jobs,
            &self.session_id,
            self.subagent_activity + self.job_activity,
        );

        vec![self.background_tasks()]
    }

    fn background_tasks(&self) -> Event {
        Event::BackgroundTasks(BackgroundTaskSnapshot {
            parent_session: BackgroundTaskKey::deepseek(&self.session_id),
            tasks: self
                .subagent_rows
                .iter()
                .chain(&self.job_rows)
                .cloned()
                .collect(),
            discovery: BackgroundTaskLoadState::Ready,
            activity: self.subagent_activity + self.job_activity,
        })
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

        vec![Event::Skills(catalogs::skill_catalog(&frame.skills))]
    }

    fn on_presets(&mut self, payload: &Value) -> Vec<Event> {
        let Some(frame) = frames::parse::<frames::PresetsFrame>(PRESETS_FRAME, payload) else {
            return Vec::new();
        };

        if frame.session_id != self.session_id {
            return Vec::new();
        }

        self.agent_preset.clone_from(&frame.current);

        let mut events = vec![Event::AgentPresets {
            presets: catalogs::preset_catalog(&frame.presets),
            current: frame.current,
        }];

        if let Some(text) = frame.refusal {
            events.push(Event::ItemStarted(Item::Error { text }));
        }

        events
    }

    /// Report what a command issued from this tab came to.
    fn on_settled(&mut self, payload: &Value) -> Vec<Event> {
        let Some(frame) = frames::parse::<frames::SettledFrame>(SETTLED_FRAME, payload) else {
            return Vec::new();
        };

        // The one answer addressed to a conversation the tab is not on yet.
        if let frames::SettledCommand::Switched = frame.command {
            self.on_switched();

            return Vec::new();
        }

        if frame.session_id != self.session_id {
            return Vec::new();
        }

        match frame.command {
            frames::SettledCommand::Switched => Vec::new(),
            // The send opened a turn on the strength of an answer still to
            // come, so the refusal is what ends it.
            frames::SettledCommand::PromptRefused {
                steering: false,
                error,
            } => vec![Event::TurnCompleted { error: Some(error) }],
            // Neither refusal below concerns a conversation change, so they
            // are shown without settling one that may be under way.
            frames::SettledCommand::PromptRefused {
                steering: true,
                error,
            }
            | frames::SettledCommand::QueueRemovalRefused { error } => vec![
                Event::QueuedPrompts(self.queued_prompts.clone()),
                Event::ItemStarted(Item::Error { text: error }),
            ],
            frames::SettledCommand::RenameRefused { error } => {
                vec![Event::ItemStarted(Item::Error { text: error })]
            }
            frames::SettledCommand::ModelSelected {
                model,
                reasoning_effort,
                error,
            } => {
                // The harness answers with the selection it committed, which
                // is what the directory records: an effort it declined to pin
                // is absent there.
                if error.is_none() {
                    self.models.set_selected(model, reasoning_effort);
                }

                vec![Event::ModelSelection {
                    model: self.models.selected().map(str::to_string),
                    effort: self.models.effort().map(str::to_string),
                    refusal: error,
                }]
            }
            frames::SettledCommand::Slash {
                name,
                arguments,
                value,
                error,
            } => {
                let outcome = match error {
                    Some(message) => SlashCommandOutcome::Rejected { message },
                    None => catalogs::command_outcome(&name, &arguments, &value),
                };

                // A refused permission switch leaves the session on its
                // earlier preset, which a picker that showed the pick ahead of
                // the answer has to be told.
                let reverted = (name == "permission"
                    && matches!(outcome, SlashCommandOutcome::Rejected { .. }))
                .then(|| self.usage.approval_presets())
                .flatten();

                let mut events = vec![Event::SlashCommandResult { name, outcome }];

                events.extend(reverted);

                events
            }
            // A conversation change is what the restore and branch flows wait
            // on, and an error event is what releases them.
            frames::SettledCommand::SwitchFailed { error } => vec![Event::Error {
                message: format!("DeepSeek could not open the conversation: {error}"),
                fatal: false,
            }],
        }
    }

    fn on_commands(&self, payload: &Value) -> Vec<Event> {
        let Some(frame) = frames::parse::<frames::CommandsFrame>(COMMANDS_FRAME, payload) else {
            return Vec::new();
        };

        if frame.session_id != self.session_id {
            return Vec::new();
        }

        vec![Event::Commands(catalogs::command_catalog(&frame.commands))]
    }

    /// The harness republishes its whole pending inbox after every change,
    /// so this replaces what the tab holds rather than amending it: an
    /// increment would have to guess at removals another client made.
    fn on_queue(&mut self, payload: &Value) -> Vec<Event> {
        let Some(frame) = frames::parse::<frames::QueueFrame>(QUEUE_FRAME, payload) else {
            return Vec::new();
        };

        self.queued_prompts = queued_prompts(&frame.items);

        vec![Event::QueuedPrompts(self.queued_prompts.clone())]
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

            // A log whose last turn never closed reads as running, which is
            // also what a turn whose end record the harness rejected leaves
            // behind. The harness's session list says which it is.
            check_running(
                self.client.clone(),
                self.session_id.clone(),
                Arc::clone(&self.deliver),
            );
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

        if let Some(message) = frame.read_error {
            events.push(Event::ItemStarted(Item::Error { text: message }));
        }

        events
    }
}

impl Session {
    pub(crate) fn expire_questions(&mut self) -> Vec<Event> {
        self.controls.retire_questions();

        self.pending_questions
            .take()
            .map(|request| Event::InputResolved {
                id: question_id(&request),
                resolution: QuestionResolution::Expired,
            })
            .into_iter()
            .collect()
    }

    pub(crate) fn control_completed(&mut self, payload: &Value) -> Vec<Event> {
        let Some(operation) = payload["id"]
            .as_u64()
            .and_then(|id| self.controls.complete(id))
        else {
            return Vec::new();
        };

        let error = payload["error"].as_str().map(str::to_string);

        let mut events = Vec::new();

        if let Some(message) = payload["stopError"].as_str() {
            events.push(Event::Error {
                message: format!("DeepSeek could not confirm stopping the turn: {message}"),
                fatal: false,
            });
        }

        match operation {
            Operation::Approval(request) => {
                if self.pending_approval.as_ref() == Some(&request) {
                    match error {
                        Some(message) => events.push(Event::Error {
                            message,
                            fatal: false,
                        }),
                        None => {
                            self.pending_approval = None;

                            events.push(Event::ApprovalResolved);
                        }
                    }
                }
            }
            Operation::Questions { request, skipped } => {
                if self.pending_questions.as_ref() == Some(&request) {
                    let id = question_id(&request);

                    match error {
                        Some(message) => events.push(Event::InputSubmissionFailed { id, message }),
                        None => {
                            self.pending_questions = None;

                            events.push(Event::InputResolved {
                                id,
                                resolution: if skipped {
                                    QuestionResolution::Skipped
                                } else {
                                    QuestionResolution::Submitted {
                                        message: None,
                                        started_turn: false,
                                    }
                                },
                            });
                        }
                    }
                }
            }
            Operation::Interrupt | Operation::InterruptChild(_) => {
                if let Some(message) = error {
                    events.push(Event::Error {
                        message,
                        fatal: false,
                    });
                }
            }
        }

        events
    }
}

impl Session {
    /// Answer the approval the harness is blocked on.
    ///
    /// The harness accepts only `allowed-once` and `rejected` from a client;
    /// `cancelled` and `unavailable` are outcomes it reaches on its own. So a
    /// request to allow for the rest of the session cannot be expressed, and a
    /// request to cancel the turn is a refusal plus a stop.
    pub fn respond_approval(&mut self, decision: &str) -> bool {
        let Some(request) = self.pending_approval.as_ref() else {
            return false;
        };

        let outcome = match decision {
            "accept" | "acceptForSession" => "allowed-once",
            _ => "rejected",
        };

        self.controls.submit(
            Operation::Approval(request.clone()),
            "$events/result",
            json!({"clientId": request.client_id, "eventId": request.event_id,
                "outcome": {"kind": "result", "value": outcome}}),
            (decision == "cancel").then(|| self.session_id.clone()),
        )
    }

    /// Admission leaves the original request answerable until its result arrives.
    pub fn respond_input(
        &mut self,
        id: &str,
        answers: Option<Vec<Vec<String>>>,
    ) -> Result<QuestionResponse, String> {
        let request = self
            .pending_questions
            .as_ref()
            .filter(|request| question_id(request) == id)
            .ok_or("This question is no longer pending.")?;

        let skipped = answers.is_none();

        let outcome = match answers {
            Some(answers) if answers.len() == request.ids.len() => {
                let answers: Vec<Value> = request
                    .ids
                    .iter()
                    .zip(answers)
                    .map(|(id, selected)| json!({"id": id, "selected": selected}))
                    .collect();

                json!({"kind": "result", "value": {"answers": answers}})
            }
            Some(_) => return Err("Complete every question before submitting.".into()),
            None => json!({"kind": "rejected", "error": {
                "name": "Error", "code": "cancelled", "message": "the user dismissed the question"
            }}),
        };

        self.controls.submit(
            Operation::Questions {
                request: request.clone(),
                skipped,
            },
            "$events/result",
            json!({"clientId": request.client_id, "eventId": request.event_id, "outcome": outcome}),
            None,
        ).then_some(QuestionResponse::Pending)
            .ok_or_else(|| "The question response could not be queued.".to_string())
    }

    /// Ask for one workflow member's conversation.
    ///
    /// A live run reports its members as they are published, so there is
    /// nothing to poll: the read happens when a member is opened, and the run's
    /// own events say when it changed.
    pub fn request_workflow_agent_transcript(&mut self, task_id: &str, agent_id: &str) {
        load_workflow_transcript(
            self.client.clone(),
            self.session_id.clone(),
            task_id.to_string(),
            agent_id.to_string(),
            Arc::clone(&self.deliver),
        );
    }

    /// Ask the harness for a fresher child-agent catalog.
    ///
    /// The catalog is a call rather than a stream, so it reports what was true
    /// when it was asked. The counter travels with it and is what stops a slow
    /// answer from replacing a newer one.
    pub fn refresh_background_tasks(&mut self) {
        self.subagent_activity += 1;

        load_subagents(
            self.client.clone(),
            self.session_id.clone(),
            self.subagent_activity,
            Arc::clone(&self.deliver),
        );
    }

    /// Ask for one child's conversation.
    ///
    /// A child this session's catalog never named cannot be addressed: the read
    /// selects a transport by the child's kind, and only the catalog reports
    /// which kind a child is.
    ///
    /// A background job has no conversation, and the harness keeps its output
    /// behind the agent's own job tools, so its detail view is answered at once
    /// as unreadable instead of waiting on a read that never starts.
    pub fn load_background_task_transcript(&mut self, child: &str) -> Vec<Event> {
        if self.job_rows.iter().any(|row| row.key.id == child) {
            return vec![Event::BackgroundTaskTranscript {
                key: BackgroundTaskKey::deepseek(child),
                update: BackgroundTaskTranscriptUpdate::state(
                    BackgroundTaskLoadState::Unavailable {
                        message: "DeepSeek Harness shares job output only with the agent that started the job.".to_string(),
                    },
                ),
            }];
        }

        let Some(continuable) = self.subagent_modes.get(child).copied() else {
            return Vec::new();
        };

        load_subagent_transcript(
            self.client.clone(),
            self.session_id.clone(),
            child.to_string(),
            continuable,
            Arc::clone(&self.deliver),
        );

        Vec::new()
    }

    /// Stop a continuable child's current turn.
    ///
    /// The request rides the parent's durable authority rather than a live
    /// parent agent, and it acknowledges the signal rather than the child
    /// having stopped, so the row can stay visibly running for a moment.
    pub fn interrupt_background_task(&mut self, child: &str) -> bool {
        let payload = json!({
            "parentSessionId": self.session_id,
            "childSessionId": child,
            "mode": "continuable",
        });

        self.controls.submit(
            Operation::InterruptChild(child.to_string()),
            "subagents/interruptByParent",
            payload,
            None,
        )
    }

    /// Point the session at another model, optionally pinning a reasoning
    /// effort. The answer arrives as [`crate::chat::Event::ModelSelection`],
    /// carrying why the harness refused when it did, because a picker that
    /// silently keeps showing a value the session never adopted is worse than
    /// an error.
    ///
    /// An absent `effort` is how the adapter's own default is asked for, which
    /// is what a model switch wants: the levels belong to the exact model, so
    /// carrying the previous one over could pin a level this route rejects.
    pub(crate) fn select_model(&mut self, model: &str, effort: Option<&str>) {
        let (provider, id) = self.models.route(model);

        let mut payload = json!({
            "sessionId": self.session_id,
            "provider": provider,
            "model": id,
        });

        if let Some(effort) = effort {
            payload["reasoningEffort"] = json!(effort);
        }

        let client = self.client.clone();
        let deliver = Arc::clone(&self.deliver);
        let session_id = self.session_id.clone();
        let model = model.to_string();

        self.lane.run(async move {
            let command = match client.request("session/selectModel", payload).await {
                Ok(selected) => json!({
                    "kind": "modelSelected", "model": model,
                    "reasoningEffort": selected["selected"]["reasoningEffort"],
                }),
                Err(error) => json!({
                    "kind": "modelSelected", "model": model, "error": error.message(),
                }),
            };

            deliver(settled(&session_id, command));
        });
    }

    /// What the session is actually set to, for a caller restoring its pickers
    /// after a refused pick.
    pub fn selection(&self) -> (Option<&str>, Option<&str>) {
        (self.models.selected(), self.models.effort())
    }

    /// Switch this conversation's permission preset.
    ///
    /// The harness exposes the switch only as its `/permission` command, and
    /// selecting the preset already in effect records nothing, so sending a
    /// pick that already holds is harmless.
    ///
    /// Nobody typed this command: it restores a remembered pick when a
    /// conversation opens. A switch that took is already visible as the
    /// permission projection it moved, so only a refusal is reported.
    pub fn select_permission(&mut self, preset: &str) {
        let client = self.client.clone();
        let deliver = Arc::clone(&self.deliver);
        let session_id = self.session_id.clone();
        let preset = preset.to_string();

        self.lane.run(async move {
            let command = run_slash(&client, &session_id, "permission", &preset).await;

            let refused = command["error"].is_string()
                || matches!(
                    catalogs::command_outcome("permission", &preset, &command["value"]),
                    SlashCommandOutcome::Rejected { .. }
                );

            if refused {
                deliver(settled(&session_id, command));
            }
        });
    }

    /// Recompose this conversation's agent from another preset.
    ///
    /// The harness allows this only while no turn has run: the logged history
    /// was produced under the previous composition's tools, and a new one may
    /// not be able to make the calls that history records. Rather than
    /// predicting that here, the preset catalog is published again either way:
    /// naming the new preset, or the one still in force beside the harness's
    /// own reason for keeping it.
    pub fn select_agent_preset(&mut self, preset: &str) {
        let payload = json!({ "agentId": self.session_id, "agentPreset": preset });

        let client = self.client.clone();
        let deliver = Arc::clone(&self.deliver);
        let session_id = self.session_id.clone();
        let previous = self.agent_preset.clone();
        let preset = preset.to_string();

        self.lane.run(async move {
            let refusal = client
                .call("agentPresets/select", payload)
                .await
                .err()
                .map(|error| error.message().to_string());

            // A preset names the plugins the agent is built from, so the
            // commands and skills it serves are the ones that just changed.
            // Leaving the palette on the previous composition's would offer
            // entries the new agent cannot run.
            if refusal.is_none() {
                load_commands(client.clone(), session_id.clone(), Arc::clone(&deliver));

                load_skills(client.clone(), session_id.clone(), Arc::clone(&deliver));
            }

            let current = if refusal.is_none() {
                Some(preset)
            } else {
                previous
            };

            load_agent_presets(client, session_id, current, refusal, deliver);
        });
    }

    /// Send a prompt and the images it carries.
    ///
    /// A message sent while a turn is running is steered into that turn rather
    /// than queued behind it, which is what makes a correction land before the
    /// work it is correcting finishes. The harness treats a steer whose window
    /// has already closed as the next queued message, so both outcomes leave
    /// the message pending and the reply is reported as steered either way.
    ///
    /// The outcome names what was asked for. Admission is the harness's to
    /// answer, and a refusal ends the turn this opened with its reason.
    pub fn send_user_message(&mut self, text: &str, images: &[MessageImage]) -> SendOutcome {
        let steering = self.running;

        let mode = if steering { "steer" } else { "queue" };

        let payload = prompt_payload(&self.session_id, text, mode, images);

        let client = self.client.clone();
        let deliver = Arc::clone(&self.deliver);
        let session_id = self.session_id.clone();

        self.lane.run(async move {
            if let Err(error) = client.request("session/prompt", payload).await {
                deliver(settled(
                    &session_id,
                    json!({
                        "kind": "promptRefused", "steering": steering, "error": error.message(),
                    }),
                ));
            }
        });

        if steering {
            SendOutcome::Steered
        } else {
            SendOutcome::StartedTurn
        }
    }

    /// Drop one prompt the harness has accepted but not started.
    ///
    /// Answers whether the removal was requested. A message the harness has
    /// already claimed is one the transcript is about to show as sent, so a
    /// refusal republishes the inbox and the row returns.
    pub fn remove_queued_prompt(&mut self, item_id: &str) -> bool {
        let payload = json!({
            "sessionId": self.session_id,
            "itemId": item_id,
            "action": { "kind": "remove" },
        });

        let client = self.client.clone();
        let deliver = Arc::clone(&self.deliver);
        let session_id = self.session_id.clone();

        self.lane.run(async move {
            if let Err(error) = client.request("session/updateQueue", payload).await {
                tracing::warn!(
                    "deepseek queued prompt could not be removed: {}",
                    error.message()
                );

                deliver(settled(
                    &session_id,
                    json!({ "kind": "queueRemovalRefused", "error": error.message() }),
                ));
            }
        });

        true
    }

    /// Pin this conversation's title.
    ///
    /// The harness normalizes what it accepts and republishes the title it
    /// keeps, which is how the tab learns the final wording. The recent list
    /// is re-read afterwards because the row it holds for this conversation
    /// still carries the old one.
    pub fn rename(&mut self, title: &str) {
        let payload = json!({ "sessionId": self.session_id, "title": title });

        let client = self.client.clone();
        let deliver = Arc::clone(&self.deliver);
        let session_id = self.session_id.clone();
        let cwd = self.cwd.clone();

        self.lane.run(async move {
            match client.request("session/rename", payload).await {
                Ok(_) => load_sessions(client, cwd, deliver),
                Err(error) => deliver(settled(
                    &session_id,
                    json!({ "kind": "renameRefused", "error": error.message() }),
                )),
            }
        });
    }

    /// Ask which prompts this conversation can be branched in front of.
    pub fn request_fork_checkpoints(&mut self) {
        load_fork_checkpoints(
            self.client.clone(),
            self.session_id.clone(),
            Arc::clone(&self.deliver),
        );
    }

    /// Branch this conversation at `anchor` and continue in the copy.
    ///
    /// The harness cuts on whole turns: it takes the anchoring seq to mean the
    /// turn that seq falls in and keeps that turn entire, which is why the
    /// anchor names the prompt ahead of the one the branch stops at. The tab
    /// then moves to the child the same way it moves to any other
    /// conversation, so the parent is left exactly as it was.
    pub fn fork(&mut self, anchor: Option<&ForkAnchor>) -> Result<(), String> {
        let at_seq = match anchor {
            Some(ForkAnchor::DeepSeekThrough(seq)) => Some(*seq),
            Some(_) => return Err("that branch point belongs to another agent".to_string()),
            None => None,
        };

        self.switch_to(Target::BranchOf {
            session_id: self.session_id.clone(),
            at_seq,
        });

        Ok(())
    }

    /// Search the conversations this tab could resume for a phrase.
    ///
    /// The read runs off the caller's thread for the same reason the recent
    /// list does: it reaches the harness's own index and a composer that waited
    /// on it would be unusable until the answer came back.
    pub fn search_sessions(&mut self, query: &str) {
        load_search(
            self.client.clone(),
            self.cwd.clone(),
            query.to_string(),
            Arc::clone(&self.deliver),
        );
    }

    /// Run one of the harness's own commands. The outcome arrives as
    /// [`crate::chat::Event::SlashCommandResult`].
    ///
    /// The registry is reached directly rather than through a prompt: the host
    /// admits a prompt to the agent whatever it starts with, so a slash line
    /// sent that way would reach the model as text instead of running.
    pub fn execute_slash_command(&mut self, name: &str, arguments: &str) -> SlashCommandOutcome {
        let client = self.client.clone();
        let deliver = Arc::clone(&self.deliver);
        let session_id = self.session_id.clone();
        let name = name.to_string();
        let arguments = arguments.to_string();

        self.lane.run(async move {
            let command = run_slash(&client, &session_id, &name, &arguments).await;

            deliver(settled(&session_id, command));
        });

        SlashCommandOutcome::Accepted
    }

    /// Stop the running turn. The harness keeps whatever the turn already
    /// streamed, so nothing is discarded here either.
    pub fn interrupt(&mut self) -> bool {
        if !self.running {
            return false;
        }

        self.controls.submit(
            Operation::Interrupt,
            "session/cancel",
            json!({"request": {"sessionId": self.session_id}}),
            None,
        )
    }

    pub fn session_id(&self) -> Option<&str> {
        Some(&self.session_id)
    }

    pub fn has_active_operation(&self) -> bool {
        self.running
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.controls.clear();

        let mut actions: Vec<CloseAction> = self
            .queued_prompts
            .drain(..)
            .filter_map(|prompt| prompt.id)
            .map(CloseAction::RemoveQueued)
            .collect();

        if self.running {
            actions.push(CloseAction::CancelTurn);
        }

        schedule_close_actions(self.client.clone(), self.session_id.clone(), actions);
    }
}
