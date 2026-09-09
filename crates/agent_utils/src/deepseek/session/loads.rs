//! Background catalog and history reads for a session.

use std::sync::Arc;
use std::thread;

use serde_json::{Value, json};

use crate::chat::{Event, QueuedPrompt};
use crate::deepseek::api::ApiClient;
use crate::deepseek::events::{self, session_address};
use crate::deepseek::models::ModelDirectory;
use crate::deepseek::session::{
    COMMANDS_FRAME, FORK_CHECKPOINT_MESSAGES, FORK_CHECKPOINTS_FRAME, HISTORY_FRAME, MODELS_FRAME,
    PRESETS_FRAME, REPLAY_MESSAGES, SEARCH_FRAME, SKILLS_FRAME, SUBAGENT_TRANSCRIPT_FRAME,
    SUBAGENTS_FRAME, WORKFLOW_TRANSCRIPT_FRAME,
};
use crate::deepseek::{commands, frames, history, settings};

/// Read the conversations this tab's directory can continue.
///
/// The result is one page: the harness returns every visible session and
/// reserves its cursor for a future version, so there is nothing further to ask
/// for and no paging to drive.
pub(super) fn load_sessions(
    client: ApiClient,
    cwd: Option<String>,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    thread::spawn(
        move || match client.call("session/list", json!({ "_request": {} })) {
            Ok(listed) => deliver(json!({
                "payload": { "type": HISTORY_FRAME, "sessions": listed, "cwd": cwd },
            })),
            Err(error) => tracing::warn!(
                "deepseek recent conversations could not be read: {}",
                error.message()
            ),
        },
    );
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
pub(in crate::deepseek) fn workflow_transcript_events(payload: &Value) -> Vec<Event> {
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

pub(in crate::deepseek) fn history_events(payload: &Value) -> Vec<Event> {
    let Some(frame) = frames::parse::<frames::HistoryFrame>(HISTORY_FRAME, payload) else {
        return Vec::new();
    };
    vec![Event::History(history::sessions(
        &frame.sessions,
        frame.cwd.as_deref(),
    ))]
}

pub(in crate::deepseek) fn search_events(payload: &Value) -> Vec<Event> {
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

pub(in crate::deepseek) fn fork_checkpoint_events(payload: &Value) -> Vec<Event> {
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
pub(super) fn load_search(
    client: ApiClient,
    cwd: Option<String>,
    query: String,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    thread::spawn(move || {
        let payload = match client.request("session/search", json!({ "query": query })) {
            Ok(matches) => match client.call("session/list", json!({ "_request": {} })) {
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
pub(super) fn load_commands(
    client: ApiClient,
    session_id: String,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
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
pub(super) fn load_skills(
    client: ApiClient,
    session_id: String,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    thread::spawn(move || {
        match client.request("skills/list", json!({ "sessionId": session_id.clone() })) {
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
pub(super) fn load_agent_presets(
    client: ApiClient,
    session_id: String,
    current: Option<String>,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    thread::spawn(move || match client.call("agentPresets/list", json!({})) {
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
pub(super) fn load_subagents(
    client: ApiClient,
    session_id: String,
    activity: u64,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    thread::spawn(move || {
        let payload = json!({ "parentSessionId": session_id });
        match client.call("subagents/list", payload) {
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
pub(super) fn load_subagent_transcript(
    client: ApiClient,
    parent_session_id: String,
    child: String,
    continuable: bool,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    thread::spawn(move || {
        let address = json!({
            "kind": "subagent",
            "parentSessionId": parent_session_id,
            "childSessionId": child,
            "mode": if continuable { "continuable" } else { "one-shot" },
        });
        match events::snapshot(&client, address, REPLAY_MESSAGES) {
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
pub(super) fn load_workflow_transcript(
    client: ApiClient,
    task_id: String,
    child: String,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    thread::spawn(move || {
        match events::snapshot(&client, session_address(&child), REPLAY_MESSAGES) {
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

/// Read the prompts this conversation can be branched in front of.
///
/// The log answers it rather than the transcript this tab happens to be
/// showing, so the offer covers turns from before the tab attached and stays
/// right after a compaction rewrites what the transcript displays. It is read
/// per request for the same reason: a list assembled as events went by would
/// describe the conversation as it was when the tab last looked.
pub(super) fn load_fork_checkpoints(
    client: ApiClient,
    session_id: String,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    thread::spawn(move || {
        // A failure is delivered rather than only logged: the picker waits on
        // this page, so a read that reported nothing would hold it open on a
        // list never arriving.
        let payload = match events::snapshot(
            &client,
            session_address(&session_id),
            FORK_CHECKPOINT_MESSAGES,
        ) {
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
pub(super) fn load_models(
    client: ApiClient,
    session_id: String,
    selected: Value,
    wanted_model: Option<String>,
    wanted_effort: Option<String>,
    declares_image_input: bool,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
) {
    thread::spawn(move || {
        let read_catalog = || {
            client
                .call("session/modelCatalog", json!({}))
                .map(|mut catalog| {
                    catalog["current"] = if selected.is_null() {
                        catalog["default"].clone()
                    } else {
                        selected.clone()
                    };
                    catalog
                })
        };
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

                match client.request("session/selectModel", payload) {
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
