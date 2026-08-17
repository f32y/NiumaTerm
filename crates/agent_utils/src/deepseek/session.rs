//! One tab's conversation on the shared host.
//!
//! Unlike the CLI-backed adapters, a session here owns no process: the host
//! outlives every tab, and this holds a session id on it plus the reader
//! threads feeding the pane.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

use serde_json::{Value, json};

use crate::background_task::{
    BackgroundTaskKey, BackgroundTaskRefs, BackgroundTaskTranscriptUpdate,
};
use crate::chat::{Event, SlashCommandOutcome, ThreadSettings};
use crate::deepseek::api::{ApiClient, CallError};
use crate::deepseek::events::Downlinks;
use crate::deepseek::host::{self, Host, HostError};
use crate::deepseek::mapping::{self, ApprovalRequest, QuestionRequest, ToolTracker};
use crate::deepseek::models::ModelDirectory;
use crate::deepseek::projections::ProjectionTracker;
use crate::deepseek::workflows::WorkflowTracker;
use crate::deepseek::{commands, history, subagents};

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
    /// The turn state this side knows about, so a stop is only offered while a
    /// turn is actually running.
    running: bool,
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
const REPLAY_FRAME: &str = "nmt/replay";
const COMMANDS_FRAME: &str = "nmt/commands";
const SUBAGENTS_FRAME: &str = "nmt/subagents";
const SUBAGENT_TRANSCRIPT_FRAME: &str = "nmt/subagent-transcript";
const SKILLS_FRAME: &str = "nmt/skills";

/// How much of a resumed conversation is rebuilt. The harness pages history at
/// whole-message boundaries, so this is a count of messages rather than of
/// events; one page is what the pane shows, and older turns stay in the log.
const REPLAY_MESSAGES: u64 = 200;

/// Open a conversation on the host, or reattach to an existing one.
///
/// The same call serves both: naming an existing id returns that session
/// unchanged when the directory matches, and refuses when it does not, which is
/// what makes reattaching safe to attempt without a separate probe.
fn open_conversation(
    client: &ApiClient,
    cwd: Option<&str>,
    session_id: Option<&str>,
) -> Result<String, String> {
    let mut payload = json!({});
    if let Some(cwd) = cwd {
        payload["cwd"] = json!(cwd);
    }
    if let Some(session_id) = session_id {
        payload["sessionId"] = json!(session_id);
    }

    let created = client
        .call("session.create", payload)
        .map_err(|error| error.message().to_string())?;

    created["sessionId"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "the harness answered without a conversation id".to_string())
}

/// Read the conversations this tab's directory can continue.
///
/// The result is one page: the harness returns every visible session and
/// reserves its cursor for a future version, so there is nothing further to ask
/// for and no paging to drive.
fn load_sessions(
    client: ApiClient,
    cwd: Option<String>,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    thread::spawn(move || match client.call("session.list", json!({})) {
        Ok(listed) => deliver(json!({
            "payload": { "type": HISTORY_FRAME, "sessions": listed, "cwd": cwd },
        })),
        Err(error) => tracing::warn!(
            "deepseek recent conversations could not be read: {}",
            error.message()
        ),
    });
}

/// Read the session's command registry and deliver the palette it fills.
///
/// Discovery is asynchronous for the same reason the model directory's is: it
/// is a call rather than a push, and a tab that waited on it would be unusable
/// until the host answered.
fn load_commands(client: ApiClient, session_id: String, deliver: Arc<dyn Fn(Value) + Send + Sync>) {
    thread::spawn(move || {
        match client.call(commands::LIST_METHOD, commands::agent_args(&session_id)) {
            Ok(listed) => deliver(json!({
                "payload": { "type": COMMANDS_FRAME, "sessionId": session_id, "commands": listed },
            })),
            Err(error) => {
                tracing::warn!("deepseek commands could not be listed: {}", error.message())
            }
        }
    });
}

/// Read the skills a prompt in this conversation can name.
fn load_skills(client: ApiClient, session_id: String, deliver: Arc<dyn Fn(Value) + Send + Sync>) {
    thread::spawn(move || {
        match client.call("skill.list", json!({ "sessionId": session_id.clone() })) {
            Ok(listed) => deliver(json!({
                "payload": { "type": SKILLS_FRAME, "sessionId": session_id, "skills": listed },
            })),
            Err(error) => {
                tracing::warn!("deepseek skills could not be listed: {}", error.message())
            }
        }
    });
}

/// Read the direct children this conversation spawned.
fn load_subagents(
    client: ApiClient,
    session_id: String,
    activity: u64,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    thread::spawn(move || {
        let payload = json!({ "parentSessionId": session_id });
        match client.call("subagent.list", payload) {
            Ok(catalog) => deliver(json!({
                "payload": {
                    "type": SUBAGENTS_FRAME,
                    "sessionId": session_id,
                    "catalog": catalog,
                    "activity": activity,
                },
            })),
            Err(error) => tracing::warn!(
                "deepseek child agents could not be listed: {}",
                error.message()
            ),
        }
    });
}

/// Read one child's own conversation.
fn load_subagent_transcript(
    client: ApiClient,
    parent_session_id: String,
    child: String,
    continuable: bool,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    thread::spawn(move || {
        let payload = json!({
            "parentSessionId": parent_session_id,
            "childSessionId": child,
            "mode": if continuable { "continuable" } else { "one-shot" },
            "maxMessages": REPLAY_MESSAGES,
        });
        match client.call("subagent.history", payload) {
            Ok(page) => deliver(json!({
                "payload": {
                    "type": SUBAGENT_TRANSCRIPT_FRAME,
                    "sessionId": parent_session_id,
                    "childSessionId": child,
                    "page": page,
                },
            })),
            Err(error) => tracing::warn!(
                "deepseek child conversation could not be read: {}",
                error.message()
            ),
        }
    });
}

/// Read a resumed conversation's tail page and deliver the turns it rebuilds.
fn load_replay(client: ApiClient, session_id: String, deliver: Arc<dyn Fn(Value) + Send + Sync>) {
    thread::spawn(move || {
        let payload = json!({ "sessionId": session_id, "maxMessages": REPLAY_MESSAGES });
        // A failure is delivered rather than only logged: the pane holds the
        // conversation picker open until the replay settles, so a read that
        // reported nothing would leave it waiting on a page never coming.
        let payload = match client.call("session.history", payload) {
            Ok(page) => json!({ "type": REPLAY_FRAME, "sessionId": session_id, "page": page }),
            Err(error) => json!({
                "type": REPLAY_FRAME,
                "sessionId": session_id,
                "error": error.message(),
            }),
        };
        deliver(json!({ "payload": payload }));
    });
}

/// Read the session's model directory and reconcile it with the profile's pick,
/// then deliver the result.
///
/// This runs off the create path because provider lookups reach the network,
/// and a tab must not wait on a slow provider before it can be typed in. The
/// selection is applied here rather than reported and applied later, so what
/// the pane displays is what the harness will actually route.
fn load_models(
    client: ApiClient,
    session_id: String,
    wanted_model: Option<String>,
    wanted_effort: Option<String>,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    thread::spawn(move || {
        let mut catalog =
            match client.call("session.models", json!({ "sessionId": session_id.clone() })) {
                Ok(catalog) => catalog,
                Err(error) => {
                    tracing::warn!(
                        "deepseek model directory could not be read: {}",
                        error.message()
                    );
                    return;
                }
            };

        if let Some(model) = wanted_model {
            let directory = ModelDirectory::parse(&catalog);
            let already = directory.selected() == Some(model.as_str())
                && (wanted_effort.is_none() || directory.effort() == wanted_effort.as_deref());

            // A profile naming a model this deployment does not serve leaves
            // the harness on its own selection, which the catalog already
            // describes; overriding it with a route that does not exist would
            // only fail the next turn.
            if let Some((provider, id)) = directory.route(&model)
                && !already
            {
                let mut payload = json!({
                    "sessionId": session_id,
                    "provider": provider,
                    "model": id,
                });
                if let Some(effort) = &wanted_effort {
                    payload["reasoningEffort"] = json!(effort);
                }

                match client.call("session.selectModel", payload) {
                    Ok(selected) => catalog["current"] = selected["selected"].clone(),
                    Err(error) => tracing::warn!(
                        "deepseek could not select the profile's model: {}",
                        error.message()
                    ),
                }
            }
        }

        deliver(json!({
            "payload": { "type": MODELS_FRAME, "sessionId": session_id, "models": catalog },
        }));
    });
}

impl Session {
    /// Create a conversation on the running host and start delivering its
    /// frames. `cwd` is the project directory this tab works in; the host's own
    /// working directory applies when it is absent.
    pub fn create(
        launch: &crate::LaunchConfig,
        cwd: Option<String>,
        deliver: impl Fn(Value) + Send + Sync + 'static,
    ) -> Result<Self, HostError> {
        let host = host::shared(launch)?;
        let client = host.client().clone();
        let session_id = open_conversation(&client, cwd.as_deref(), None).map_err(|error| {
            HostError::FailedToStart(format!(
                "the harness could not open a conversation: {error}"
            ))
        })?;

        // Opening the downlinks after the session exists means its first frames
        // cannot be missed: the stream replays a baseline for every attached
        // session when it opens.
        let deliver: Arc<dyn Fn(Value) + Send + Sync> = Arc::new(deliver);
        let downlinks = {
            let deliver = Arc::clone(&deliver);
            Downlinks::open(host.base(), Arc::downgrade(&host), move |frame| {
                deliver(frame)
            })
        };

        load_models(
            client.clone(),
            session_id.clone(),
            launch.model.clone(),
            launch.effort.clone(),
            Arc::clone(&deliver),
        );
        // The list is read now rather than when the picker opens, because the
        // picker refuses to open on an empty list and cannot wait for one.
        load_sessions(client.clone(), cwd.clone(), Arc::clone(&deliver));
        load_commands(client.clone(), session_id.clone(), Arc::clone(&deliver));
        load_skills(client.clone(), session_id.clone(), Arc::clone(&deliver));
        // A new conversation has no turns to replay, but its tail page still
        // carries the projection baseline the pane's gauges start from.
        load_replay(client.clone(), session_id.clone(), Arc::clone(&deliver));

        Ok(Self {
            client,
            session_id,
            cwd,
            host,
            _downlinks: downlinks,
            deliver,
            model: launch.model.clone(),
            effort: launch.effort.clone(),
            running: false,
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
    /// The tab keeps its host and both downlinks: the mux stream is aggregated
    /// across every attached session, so switching which one this tab owns is a
    /// change of address rather than a reconnection. Returns whether the
    /// harness attached, because a session rooted in another directory is one
    /// this tab cannot adopt.
    pub fn resume_thread(&mut self, thread_id: &str) -> bool {
        match open_conversation(&self.client, self.cwd.as_deref(), Some(thread_id)) {
            Ok(session_id) => {
                self.session_id = session_id;
                // Everything below describes the conversation this tab just
                // left; carrying it over would attribute it to the new one.
                self.running = false;
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
                    self.model.clone(),
                    self.effort.clone(),
                    Arc::clone(&self.deliver),
                );
                load_replay(
                    self.client.clone(),
                    self.session_id.clone(),
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

    /// The host's own diagnostics, for explaining an exit nobody asked for.
    pub fn host_diagnostics(&self) -> String {
        self.host.stderr_tail()
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

        if frame["payload"]["type"] == SUBAGENTS_FRAME {
            let payload = &frame["payload"];
            let activity = payload["activity"].as_u64().unwrap_or_default();
            // Several reads can be in flight, and an older answer describes a
            // moment the panel has already moved past.
            if payload["sessionId"].as_str() != Some(&self.session_id)
                || activity < self.subagent_activity
            {
                return Vec::new();
            }
            let snapshot = subagents::snapshot(&payload["catalog"], &self.session_id, activity);
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
            return vec![Event::BackgroundTasks(snapshot)];
        }

        if frame["payload"]["type"] == SUBAGENT_TRANSCRIPT_FRAME {
            let payload = &frame["payload"];
            if payload["sessionId"].as_str() != Some(&self.session_id) {
                return Vec::new();
            }
            let Some(child) = payload["childSessionId"].as_str() else {
                return Vec::new();
            };
            return vec![Event::BackgroundTaskTranscript {
                key: BackgroundTaskKey::deepseek(child),
                update: BackgroundTaskTranscriptUpdate::loaded(subagents::transcript(
                    &payload["page"],
                )),
            }];
        }

        if frame["payload"]["type"] == SKILLS_FRAME {
            if frame["payload"]["sessionId"].as_str() != Some(&self.session_id) {
                return Vec::new();
            }
            return vec![Event::Skills(commands::skills(&frame["payload"]["skills"]))];
        }

        if frame["payload"]["type"] == COMMANDS_FRAME {
            // A registry read for the conversation this tab has since left
            // describes an agent it no longer talks to.
            if frame["payload"]["sessionId"].as_str() != Some(&self.session_id) {
                return Vec::new();
            }
            return vec![Event::Commands(commands::catalog(
                &frame["payload"]["commands"],
            ))];
        }

        if frame["payload"]["type"] == HISTORY_FRAME {
            let payload = &frame["payload"];
            return vec![Event::History(history::sessions(
                &payload["sessions"],
                payload["cwd"].as_str(),
            ))];
        }

        if frame["payload"]["type"] == REPLAY_FRAME {
            // A page belonging to the conversation this tab has since left
            // would replace the visible transcript with another one's.
            if frame["payload"]["sessionId"].as_str() != Some(&self.session_id) {
                return Vec::new();
            }
            if let Some(message) = frame["payload"]["error"].as_str() {
                return vec![Event::Error {
                    message: message.to_string(),
                    // The conversation was attached before the page was read,
                    // so the tab works; only its earlier turns are missing.
                    fatal: false,
                }];
            }

            let page = &frame["payload"]["page"];
            // The tail page is also where a projection's current value can be
            // read: a live push reports only what changed after the tab
            // attached, so accounting and the permission preset would otherwise
            // stay blank until one of them happened to move.
            let mut events = self.usage.apply_baseline(&page["projections"]["values"]);
            events.push(Event::Replay(history::replay(page)));
            return events;
        }

        if frame["payload"]["type"] == MODELS_FRAME {
            self.models = ModelDirectory::parse(&frame["payload"]["models"]);
            // The catalog and the selection travel together, so the pickers
            // gain their options and their current value in one repaint.
            return vec![
                Event::Models(self.models.catalog()),
                Event::Ready(ThreadSettings {
                    model: self.models.selected().map(str::to_string),
                    effort: self.models.effort().map(str::to_string),
                    ..ThreadSettings::default()
                }),
            ];
        }

        // A child announces itself in the parent's own log, and the catalog is
        // a call the pane would otherwise have no reason to make: the panel
        // that would ask for one is hidden until a child is known to exist.
        // A finished turn re-reads it because a child's activity is sampled
        // when asked rather than pushed.
        let event_type = frame["payload"]["event"]["type"].as_str();
        if frame["payload"]["sessionId"].as_str() == Some(&self.session_id)
            && (event_type == Some("subagent/descriptor")
                || (event_type == Some("turn/end") && !self.subagent_modes.is_empty()))
        {
            self.refresh_background_tasks();
        }

        let mut events = mapping::map_frame(&frame, &self.session_id, &mut self.tools);

        // Workflow rows are folded from the log rather than mapped one event to
        // one row, so they are published beside whatever else the frame
        // produced instead of through the transcript vocabulary.
        if frame["payload"]["sessionId"].as_str() == Some(&self.session_id)
            && self.workflows.apply(&frame["payload"]["event"])
        {
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

    /// Answer the approval the harness is blocked on.
    ///
    /// The harness accepts only `allowed-once` and `rejected` from a client;
    /// `cancelled` and `unavailable` are outcomes it reaches on its own. So a
    /// request to allow for the rest of the session cannot be expressed, and a
    /// request to cancel the turn is a refusal plus a stop.
    pub fn respond_approval(&mut self, decision: &str) {
        let Some(request) = self.pending_approval.take() else {
            return;
        };

        let outcome = match decision {
            "accept" | "acceptForSession" => "allowed-once",
            _ => "rejected",
        };

        if let Err(error) = self.client.respond(
            &request.rpc_id,
            json!({
                "sessionId": self.session_id,
                "approvalId": request.approval_id,
                "outcome": outcome,
            }),
        ) {
            // Nothing else reports this: a refused answer leaves the turn
            // waiting exactly as an unanswered one does.
            tracing::warn!(
                "deepseek approval answer was not accepted: {}",
                error.message()
            );
        }

        if decision == "cancel" {
            self.interrupt();
        }
    }

    /// Answer the question batch the harness is blocked on, or dismiss it when
    /// `answers` is `None`.
    ///
    /// The harness validates the batch as a whole against what it asked: one
    /// answer per question, in ask order, carrying only labels it offered. So a
    /// batch that does not line up is dropped here rather than sent to be
    /// rejected, which would leave the turn waiting with the card already gone.
    pub fn respond_questions(&mut self, answers: Option<Vec<Vec<String>>>) {
        let Some(request) = self.pending_questions.take() else {
            return;
        };

        let result = match answers {
            Some(answers) if answers.len() == request.ids.len() => {
                let answers: Vec<Value> = request
                    .ids
                    .iter()
                    .zip(answers)
                    .map(|(id, selected)| json!({ "id": id, "selected": selected }))
                    .collect();
                self.client.respond(
                    &request.rpc_id,
                    json!({
                        "sessionId": self.session_id,
                        "answer": { "answers": answers },
                    }),
                )
            }
            Some(answers) => {
                tracing::warn!(
                    "deepseek question answers covered {} of {} questions and were dropped",
                    answers.len(),
                    request.ids.len(),
                );
                return;
            }
            None => self.client.respond_cancelled(&request.rpc_id),
        };

        if let Err(error) = result {
            // Nothing else reports this: a refused answer leaves the turn
            // waiting exactly as an unanswered one does.
            tracing::warn!(
                "deepseek question answer was not accepted: {}",
                error.message()
            );
        }
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
    pub fn load_background_task_transcript(&mut self, child: &str) {
        let Some(continuable) = self.subagent_modes.get(child).copied() else {
            return;
        };

        load_subagent_transcript(
            self.client.clone(),
            self.session_id.clone(),
            child.to_string(),
            continuable,
            Arc::clone(&self.deliver),
        );
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

        match self.client.call("subagent.interrupt", payload) {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(
                    "deepseek child agent could not be stopped: {}",
                    error.message()
                );
                false
            }
        }
    }

    /// Point the session at another model, optionally pinning a reasoning
    /// effort. Returns why the harness refused, because a picker that silently
    /// keeps showing a value the session never adopted is worse than an error.
    ///
    /// An absent `effort` is how the adapter's own default is asked for, which
    /// is what a model switch wants: the levels belong to the exact model, so
    /// carrying the previous one over could pin a level this route rejects.
    pub fn select_model(&mut self, model: &str, effort: Option<&str>) -> Result<(), String> {
        let Some((provider, id)) = self.models.route(model) else {
            return Err(format!("{model} is not one of this session's models"));
        };

        let mut payload = json!({
            "sessionId": self.session_id,
            "provider": provider,
            "model": id,
        });
        if let Some(effort) = effort {
            payload["reasoningEffort"] = json!(effort);
        }

        let selected = self
            .client
            .call("session.selectModel", payload)
            .map_err(|error| error.message().to_string())?;

        // The harness answers with the selection it committed, which is what
        // the directory records: an effort it declined to pin is absent there.
        self.models.set_selected(
            model.to_string(),
            selected["selected"]["reasoningEffort"]
                .as_str()
                .map(str::to_string),
        );
        Ok(())
    }

    /// What the session is actually set to, for a caller restoring its pickers
    /// after a refused pick.
    pub fn selection(&self) -> (Option<&str>, Option<&str>) {
        (self.models.selected(), self.models.effort())
    }

    /// Send a prompt. Returns why it was refused, so the composer can keep the
    /// text the user typed rather than losing it to a failed send.
    pub fn send_user_message(&mut self, text: &str) -> Result<(), String> {
        self.prompt(text)
            .map(|_| ())
            .map_err(|error| error.message().to_string())
    }

    /// Run one of the harness's own commands.
    ///
    /// The registry is reached directly rather than through a prompt: the host
    /// admits a prompt to the agent whatever it starts with, so a slash line
    /// sent that way would reach the model as text instead of running.
    pub fn execute_slash_command(&mut self, name: &str, arguments: &str) -> SlashCommandOutcome {
        let line = match arguments.trim() {
            "" => format!("/{name}"),
            arguments => format!("/{name} {arguments}"),
        };

        match self.client.call(
            commands::EXECUTE_METHOD,
            commands::execute_args(&self.session_id, &line),
        ) {
            Ok(value) => commands::outcome(name, &value),
            Err(error) => SlashCommandOutcome::Rejected {
                message: error.message().to_string(),
            },
        }
    }

    fn prompt(&self, text: &str) -> Result<Value, CallError> {
        self.client.call(
            "session.prompt",
            json!({
                "sessionId": self.session_id,
                "mode": "queue",
                "content": [{ "type": "text", "text": text }],
            }),
        )
    }

    /// Stop the running turn. The harness keeps whatever the turn already
    /// streamed, so nothing is discarded here either.
    pub fn interrupt(&mut self) {
        if !self.running {
            return;
        }

        if let Err(error) = self
            .client
            .call("session.cancel", json!({ "sessionId": &self.session_id }))
        {
            tracing::warn!("deepseek turn could not be stopped: {}", error.message());
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        Some(&self.session_id)
    }

    pub fn has_active_operation(&self) -> bool {
        self.running
    }
}
