//! One tab's conversation on the shared host.
//!
//! Unlike the CLI-backed adapters, a session here owns no process: the host
//! outlives every tab, and this holds a session id on it plus the reader
//! threads feeding the pane.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::{Value, json};

use crate::background_task::{
    BackgroundTaskKey, BackgroundTaskRefs, BackgroundTaskTranscriptUpdate,
};
use crate::chat::{
    Event, ForkAnchor, MessageImage, QueuedPrompt, SendOutcome, SlashCommandArguments,
    SlashCommandInfo, SlashCommandOutcome, SlashCommandRunPolicy, SlashCommandSource,
    ThreadSettings,
};
use crate::deepseek::api::{ApiClient, CallError};
use crate::deepseek::close::{CloseAction, schedule_close_actions};
use crate::deepseek::events::Downlinks;
use crate::deepseek::host::{self, Host, HostError};
use crate::deepseek::mapping::{self, ApprovalRequest, QuestionRequest, ToolTracker};
use crate::deepseek::models::ModelDirectory;
use crate::deepseek::projections::ProjectionTracker;
use crate::deepseek::workflows::WorkflowTracker;
use crate::deepseek::{commands, frames, history, presets, settings, subagents};
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
        .call("session.create", session_create_payload(cwd, session_id))
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

/// Read a pending-inbox snapshot into the prompts a composer can show.
///
/// A `context` occurrence is something the harness inserted for the model and
/// stays invisible until it is claimed, so it is not one of the user's own
/// pending messages and listing it would describe work nobody queued.
pub(crate) fn queued_prompts(items: &Value) -> Vec<QueuedPrompt> {
    items
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| matches!(item["placement"].as_str(), Some("queued" | "steering")))
        .filter_map(|item| {
            let text: String = item["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("");

            (!text.trim().is_empty()).then(|| QueuedPrompt {
                id: item["id"].as_str().map(str::to_string),
                text,
            })
        })
        .collect()
}

/// Frame decoders with no session state of their own: each turns one bridge
/// frame into the events it announces. They are addressed to this tab by the
/// request that provoked them, so they carry no session id to check.
pub(super) fn workflow_transcript_events(payload: &Value) -> Vec<Event> {
    let Some(frame) =
        frames::parse::<frames::WorkflowTranscriptFrame>(WORKFLOW_TRANSCRIPT_FRAME, payload)
    else {
        return Vec::new();
    };
    vec![Event::WorkflowAgentTranscript {
        task_id: frame.task_id,
        agent_id: frame.agent_id,
        items: history::items(&frame.page),
    }]
}

pub(super) fn history_events(payload: &Value) -> Vec<Event> {
    let Some(frame) = frames::parse::<frames::HistoryFrame>(HISTORY_FRAME, payload) else {
        return Vec::new();
    };
    vec![Event::History(history::sessions(
        &frame.sessions,
        frame.cwd.as_deref(),
    ))]
}

pub(super) fn search_events(payload: &Value) -> Vec<Event> {
    let Some(frame) = frames::parse::<frames::SearchFrame>(SEARCH_FRAME, payload) else {
        return Vec::new();
    };
    if let Some(message) = frame.error {
        return vec![Event::Error {
            message,
            // Only the search failed; the conversation is untouched.
            fatal: false,
        }];
    }
    vec![Event::SessionSearchResults(history::search_results(
        &frame.matches,
        &frame.sessions,
        frame.cwd.as_deref(),
    ))]
}

pub(super) fn fork_checkpoint_events(payload: &Value) -> Vec<Event> {
    let Some(frame) =
        frames::parse::<frames::ForkCheckpointsFrame>(FORK_CHECKPOINTS_FRAME, payload)
    else {
        return Vec::new();
    };
    vec![Event::ForkCheckpoints(match frame.error {
        Some(message) => Err(message),
        None => Ok(history::fork_checkpoints(&frame.page)),
    })]
}

/// Run one content search and deliver the conversations it matched.
///
/// The list is read alongside the search because the search answers with ids
/// and excerpts only; everything a row displays comes from the list, and the
/// two have to describe the same moment for the join to be complete.
fn load_search(
    client: ApiClient,
    cwd: Option<String>,
    query: String,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    thread::spawn(move || {
        let payload = match client.call("session.search", json!({ "query": query })) {
            Ok(matches) => match client.call("session.list", json!({})) {
                Ok(listed) => json!({
                    "type": SEARCH_FRAME,
                    "matches": matches,
                    "sessions": listed,
                    "cwd": cwd,
                }),
                Err(error) => json!({ "type": SEARCH_FRAME, "error": error.message() }),
            },
            Err(error) => json!({ "type": SEARCH_FRAME, "error": error.message() }),
        };
        // A failure is delivered rather than only logged: the search takes over
        // the recent list, so one that reported nothing would leave the user
        // looking at rows that no longer answer the question they asked.
        deliver(json!({ "payload": payload }));
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

/// Read the agent compositions this deployment offers, and which one built this
/// conversation.
///
/// The roster belongs to the deployment rather than to the session, but the
/// current pick belongs to the session, so both are read here: reattaching to a
/// conversation composed from another preset has to move the picker with it.
fn load_agent_presets(
    client: ApiClient,
    session_id: String,
    current: Option<String>,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    thread::spawn(move || match client.call("agentPreset.list", json!({})) {
        Ok(listed) => deliver(json!({
            "payload": {
                "type": PRESETS_FRAME,
                "sessionId": session_id,
                "presets": listed["presets"],
                "current": current,
            },
        })),
        Err(error) => {
            tracing::warn!(
                "deepseek agent presets could not be listed: {}",
                error.message()
            )
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

/// Read one workflow member's own conversation.
///
/// A member is published as a session of its own, so its log is read the same
/// way any session's is. That is deliberately not the child-agent read: the
/// catalog that read is addressed through covers what a turn delegated, and a
/// workflow member reached through it would depend on the workflow tool
/// registering there as well.
fn load_workflow_transcript(
    client: ApiClient,
    task_id: String,
    child: String,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    thread::spawn(move || {
        let payload = json!({ "sessionId": child, "maxMessages": REPLAY_MESSAGES });
        match client.call("session.history", payload) {
            Ok(page) => deliver(json!({
                "payload": {
                    "type": WORKFLOW_TRANSCRIPT_FRAME,
                    "taskId": task_id,
                    "agentId": child,
                    "page": page,
                },
            })),
            Err(error) => tracing::warn!(
                "deepseek workflow member conversation could not be read: {}",
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

/// Read the prompts this conversation can be branched in front of.
///
/// The log answers it rather than the transcript this tab happens to be
/// showing, so the offer covers turns from before the tab attached and stays
/// right after a compaction rewrites what the transcript displays. It is read
/// per request for the same reason: a list assembled as events went by would
/// describe the conversation as it was when the tab last looked.
fn load_fork_checkpoints(
    client: ApiClient,
    session_id: String,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    thread::spawn(move || {
        let payload = json!({ "sessionId": session_id, "maxMessages": FORK_CHECKPOINT_MESSAGES });
        // A failure is delivered rather than only logged: the picker waits on
        // this page, so a read that reported nothing would hold it open on a
        // list never arriving.
        let payload = match client.call("session.history", payload) {
            Ok(page) => json!({ "type": FORK_CHECKPOINTS_FRAME, "page": page }),
            Err(error) => json!({
                "type": FORK_CHECKPOINTS_FRAME,
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
    declares_image_input: bool,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    thread::spawn(move || {
        let read_catalog = || client.call("session.models", json!({ "sessionId": &session_id }));
        let mut catalog = match read_catalog() {
            Ok(catalog) => catalog,
            Err(error) => {
                tracing::warn!(
                    "deepseek model directory could not be read: {}",
                    error.message()
                );
                return;
            }
        };

        // Why the harness kept its own selection, when it did. The levels a
        // route serves are the adapter's, so a profile can name one this
        // deployment does not offer, and nothing else would ever say so: this
        // path runs in the background with no control waiting on an answer.
        let mut refusal = None;

        if let Some(model) = wanted_model {
            let mut directory = ModelDirectory::parse(&catalog);

            if declares_image_input {
                let (provider, id) = directory.route(&model);
                let (provider, id) = (provider.to_string(), id.to_string());

                match settings::declare_image_input(&client, &provider, &id) {
                    // A catalog that gained an entry is a different catalog:
                    // the model now has a name and a reasoning-effort list
                    // instead of the bare id a selection alone would show.
                    Ok(true) => {
                        if let Ok(refreshed) = read_catalog() {
                            catalog = refreshed;
                            directory = ModelDirectory::parse(&catalog);
                        }
                    }
                    Ok(false) => {}
                    // Reported beside the picker rather than only logged: the
                    // alternative is a switch that looks applied while the
                    // first message carrying an image is refused for a reason
                    // the pane never mentions.
                    Err(message) => {
                        tracing::warn!(
                            "deepseek could not declare {id} as image-capable: {message}"
                        );
                        refusal = Some(message);
                    }
                }
            }

            let already = directory.selected() == Some(model.as_str())
                && (wanted_effort.is_none() || directory.effort() == wanted_effort.as_deref());

            // A model the catalog never listed is still applied: a provider
            // resolves an unadvertised id as a text-only model on its own
            // route, which is how a profile names a model behind a proxy or one
            // the endpoint stopped advertising.
            if !already {
                let (provider, id) = directory.route(&model);
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
                    Err(error) => refusal = Some(error.message().to_string()),
                }
            }
        }

        let mut payload =
            json!({ "type": MODELS_FRAME, "sessionId": session_id, "models": catalog });
        if let Some(message) = refusal {
            payload["error"] = json!(message);
        }
        deliver(json!({ "payload": payload }));
    });
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
    /// The tab keeps its host and both downlinks: the mux stream is aggregated
    /// across every attached session, so switching which one this tab owns is a
    /// change of address rather than a reconnection. Returns whether the
    /// harness attached, because a session rooted in another directory is one
    /// this tab cannot adopt.
    pub fn resume_thread(&mut self, thread_id: &str) -> bool {
        match open_conversation(&self.client, self.cwd.as_deref(), Some(thread_id)) {
            Ok(opened) => {
                self.session_id = opened.session_id;
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
                    self.model.clone(),
                    self.effort.clone(),
                    self.declares_image_input,
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
        let mut events = self.usage.apply_baseline(&page["projections"]["values"]);

        // A run's rows are folded from the same events the log carries, so
        // a resumed conversation rebuilds them from its own history rather
        // than from a record kept beside it.
        let mut folded = false;
        for entry in page["events"].as_array().into_iter().flatten() {
            folded |= self.workflows.apply(&entry["event"]);
        }
        if folded {
            events.push(Event::Workflows(self.workflows.snapshot(&self.session_id)));
        }

        events.push(Event::Replay(history::replay(page)));
        events
    }

    fn on_models(&mut self, payload: &Value) -> Vec<Event> {
        let Some(frame) = frames::parse::<frames::ModelsFrame>(MODELS_FRAME, payload) else {
            return Vec::new();
        };
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

    /// Ask for one workflow member's conversation.
    ///
    /// A live run reports its members as they are published, so there is
    /// nothing to poll: the read happens when a member is opened, and the run's
    /// own events say when it changed.
    pub fn request_workflow_agent_transcript(&mut self, task_id: &str, agent_id: &str) {
        load_workflow_transcript(
            self.client.clone(),
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
        let (provider, id) = self.models.route(model);

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

    /// Recompose this conversation's agent from another preset.
    ///
    /// The harness allows this only while no turn has run: the logged history
    /// was produced under the previous composition's tools, and a new one may
    /// not be able to make the calls that history records. Rather than
    /// predicting that here, the refusal is returned for the picker to show —
    /// the harness owns the rule and answers with its own reason.
    pub fn select_agent_preset(&mut self, preset: &str) -> Result<(), String> {
        let payload = json!({ "sessionId": self.session_id, "agentPreset": preset });

        self.client
            .call("agentPreset.select", payload)
            .map_err(|error| error.message().to_string())?;

        // A preset names the plugins the agent is built from, so the commands
        // and skills it serves are the ones that just changed. Leaving the
        // palette on the previous composition's would offer entries the new
        // agent cannot run.
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

        Ok(())
    }

    /// Send a prompt and the images it carries.
    ///
    /// A message sent while a turn is running is steered into that turn rather
    /// than queued behind it, which is what makes a correction land before the
    /// work it is correcting finishes. The harness treats a steer whose window
    /// has already closed as the next queued message, so both outcomes leave
    /// the message pending and the reply is reported as steered either way.
    pub fn send_user_message(&mut self, text: &str, images: &[MessageImage]) -> SendOutcome {
        let steering = self.running;
        let mode = if steering { "steer" } else { "queue" };

        match self.prompt(text, mode, images) {
            Ok(_) if steering => SendOutcome::Steered,
            Ok(_) => SendOutcome::StartedTurn,
            Err(error) => SendOutcome::Rejected {
                message: error.message().to_string(),
            },
        }
    }

    /// Drop one prompt the harness has accepted but not started.
    ///
    /// Returns whether the harness took the removal, because a message it has
    /// already claimed is one the transcript is about to show as sent and the
    /// row must not disappear as though it never was.
    pub fn remove_queued_prompt(&mut self, item_id: &str) -> bool {
        let payload = json!({
            "sessionId": self.session_id,
            "itemId": item_id,
            "action": { "kind": "remove" },
        });

        match self.client.call("session.updateQueue", payload) {
            Ok(_) => {
                self.queued_prompt_ids.retain(|queued| queued != item_id);
                true
            }
            Err(error) => {
                tracing::warn!(
                    "deepseek queued prompt could not be removed: {}",
                    error.message()
                );
                false
            }
        }
    }

    /// Pin this conversation's title.
    ///
    /// The harness normalizes what it accepts and regenerates a title it chose
    /// itself, so the accepted title is read back rather than assumed. The
    /// recent list is re-read afterwards because the row it holds for this
    /// conversation still carries the old one.
    pub fn rename(&mut self, title: &str) -> Result<String, String> {
        let payload = json!({ "sessionId": self.session_id, "title": title });
        let renamed = self
            .client
            .call("session.rename", payload)
            .map_err(|error| error.message().to_string())?;

        load_sessions(
            self.client.clone(),
            self.cwd.clone(),
            Arc::clone(&self.deliver),
        );

        Ok(renamed["title"]
            .as_str()
            .unwrap_or(title)
            .trim()
            .to_string())
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
    /// anchor names the prompt ahead of the one the branch stops at. Omitting
    /// it falls back to the last completed turn. The tab then moves to the
    /// child the same way it moves to any other conversation, so the parent is
    /// left exactly as it was.
    pub fn fork(&mut self, anchor: Option<&ForkAnchor>) -> Result<(), String> {
        let at_seq = match anchor {
            Some(ForkAnchor::DeepSeekThrough(seq)) => Some(*seq),
            Some(_) => return Err("that branch point belongs to another agent".to_string()),
            None => None,
        };

        let mut payload = json!({ "sessionId": self.session_id });
        if let Some(at_seq) = at_seq {
            payload["atSeq"] = json!(at_seq);
        }

        let forked = self
            .client
            .call("session.fork", payload)
            .map_err(|error| error.message().to_string())?;

        let child = forked["sessionId"]
            .as_str()
            .ok_or_else(|| "the harness answered without a conversation id".to_string())?
            .to_string();

        self.resume_thread(&child)
            .then_some(())
            .ok_or_else(|| "the harness would not open the branched conversation".to_string())
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

        let mut answer = self.client.call(
            commands::EXECUTE_METHOD,
            commands::execute_args(&self.session_id, &line),
        );
        if answer
            .as_ref()
            .is_err_and(|error| error.message().contains(commands::UNEXPECTED_IMAGES))
        {
            answer = self.client.call(
                commands::EXECUTE_METHOD,
                commands::execute_args_without_images(&self.session_id, &line),
            );
        }

        match answer {
            Ok(value) => commands::outcome(name, &value),
            Err(error) => SlashCommandOutcome::Rejected {
                message: error.message().to_string(),
            },
        }
    }

    /// Image bytes travel inline rather than by reference: the harness's
    /// attachment method reads what a conversation already holds and is no
    /// route for putting something into one. A model that declines image
    /// input refuses the whole prompt, which is a business error the composer
    /// reports, so nothing is dropped silently to make a message fit.
    fn prompt(&self, text: &str, mode: &str, images: &[MessageImage]) -> Result<Value, CallError> {
        let mut content = vec![json!({ "type": "text", "text": text })];
        content.extend(images.iter().map(|image| {
            json!({
                "type": "image",
                "mediaType": image.media_type,
                "data": BASE64_STANDARD.encode(&image.bytes),
            })
        }));

        self.client.call(
            "session.prompt",
            json!({
                "sessionId": self.session_id,
                "mode": mode,
                "content": content,
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

impl Drop for Session {
    fn drop(&mut self) {
        let mut actions: Vec<CloseAction> = self
            .queued_prompt_ids
            .drain(..)
            .map(CloseAction::RemoveQueued)
            .collect();
        if self.running {
            actions.push(CloseAction::CancelTurn);
        }
        schedule_close_actions(self.client.clone(), self.session_id.clone(), actions);
    }
}
