//! Readers of the Remote API's catalog results: the agent compositions a
//! deployment offers, the child agents a conversation spawned, and the
//! harness's own slash commands and skills. Each is a pure function over one
//! result; the session owns the calls.
//!
//! A preset names the plugins a conversation's agent is built from, so it
//! decides what tools that conversation can ever call. The roster belongs to
//! the deployment rather than to this application, which is why it is read
//! rather than written here.

use serde_json::{Value, json};

use crate::background_task::{
    BackgroundTaskDiscoveryState, BackgroundTaskKey, BackgroundTaskKind, BackgroundTaskRefs,
    BackgroundTaskSnapshot, BackgroundTaskState, BackgroundTaskSummary,
};
use crate::chat::{
    AgentPreset, SkillCatalog, SkillInfo, SlashCommandArguments, SlashCommandInfo,
    SlashCommandOutcome, SlashCommandRunPolicy, SlashCommandSource,
};
use crate::dsh::api::{ApiClient, CallError};

/// Read an `agentPreset.list` result.
///
/// A preset that cannot compose a session stays on the harness's own roster —
/// its directory still occupies the id, and the harness's authoring surface has
/// to be able to show and delete it — but this picker only selects, so offering
/// one here would trade a visible reason now for a failed conversation later.
pub(crate) fn preset_catalog(value: &Value) -> Vec<AgentPreset> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter(|preset| preset["broken"].as_str().is_none())
        .filter_map(|preset| {
            let id = preset["id"].as_str()?;

            Some(AgentPreset {
                value: id.to_string(),
                // A preset publishes a display name only if its author wrote
                // one, and the id is what the roster is keyed by regardless.
                label: preset["name"].as_str().unwrap_or(id).to_string(),
                description: description(preset),
            })
        })
        .collect()
}

/// What the preset is for, prefixed for a locally authored one.
///
/// Trust is worth stating because a `user` preset is exactly as privileged as
/// the plugins it names: it was not vetted by the deployment, and a row that
/// presented it like a shipped one would imply it was.
fn description(preset: &Value) -> Option<String> {
    let published = preset["description"]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty());

    if preset["trust"].as_str() != Some("user") {
        return published.map(str::to_string);
    }

    Some(match published {
        Some(text) => format!("Locally authored — {text}"),
        None => "Locally authored".to_string(),
    })
}

// The child agents a conversation spawned.
//
// The harness keeps children as sessions of their own and answers for the
// direct level only, so a row describes one child rather than a subtree. Both
// reads are pure functions over a unary result; the session owns the calls.

/// Read a `subagent.list` result into the snapshot the panel renders.
///
/// A diagnostic row names a child the harness could not read; it is dropped
/// rather than shown, because nothing about it can be opened and a row that
/// only reports its own unreadability is noise beside working children.
pub(crate) fn subagent_snapshot(
    value: &Value,
    parent_session_id: &str,
    activity: u64,
) -> BackgroundTaskSnapshot {
    let parent_session = BackgroundTaskKey::deepseek(parent_session_id);

    let tasks = value["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["kind"].as_str() == Some("child"))
        .filter_map(|entry| task_summary(entry, &parent_session, parent_session_id, activity))
        .collect();

    BackgroundTaskSnapshot {
        parent_session,
        tasks,
        discovery: BackgroundTaskDiscoveryState::Ready,
        activity,
    }
}

fn task_summary(
    entry: &Value,
    parent_session: &BackgroundTaskKey,
    parent_session_id: &str,
    activity: u64,
) -> Option<BackgroundTaskSummary> {
    let id = entry["id"].as_str()?;
    let continuable = entry["mode"].as_str() == Some("continuable");

    // The harness samples whether the child's driver is running; it
    // reports no failure state, so an inactive child reads as finished
    // rather than as one whose outcome is known.
    let running = entry["activity"].as_str() == Some("running");

    Some(BackgroundTaskSummary {
        key: BackgroundTaskKey::deepseek(id),
        parent_session: parent_session.clone(),
        refs: BackgroundTaskRefs::DeepSeek {
            parent_session_id: parent_session_id.to_string(),
            continuable,
        },
        kind: BackgroundTaskKind::Agent,
        display_name: entry["label"].as_str().map(str::to_string),
        agent_type: None,
        objective: None,
        status: None,
        state: if running {
            BackgroundTaskState::Working
        } else {
            BackgroundTaskState::Done
        },
        sequence: activity,
        started_at: None,
        updated_at: None,
        completed_at: None,
        model: None,
        // The catalog covers the direct level only, so every row here
        // is one step below the conversation that asked for it.
        depth: Some(1),
        last_preview: None,
        // Interrupting reaches a continuable child through its parent's
        // authority; a one-shot child is one execution with nothing to
        // stop between its start and its result.
        can_stop: continuable && running,
    })
}

// The harness's own slash commands.
//
// The Remote gateway resolves each command against the addressed Agent.

/// Gateway endpoint listing one session's effective commands.
pub(crate) const COMMAND_LIST_METHOD: &str = "commands/list";

/// Gateway endpoint running one command line.
///
/// Sending the line as an ordinary prompt does not run it: the host admits a
/// prompt to the agent whatever it starts with, so a slash line delivered that
/// way reaches the model as text.
const EXECUTE_METHOD: &str = "commands/execute";

/// Named arguments for a gateway call addressed to one session's agent.
///
/// The registry resolves the agent from a session id, and the argument is named
/// by the resolver that does the resolving rather than by the method's own
/// parameter, so the two names differ.
pub(crate) fn agent_args(session_id: &str) -> Value {
    json!({ "agentId": session_id })
}

/// Run one command line addressed to a session's agent.
///
/// The attachment list is required even when empty. Older hosts name it
/// `images`; retry only their exact argument rejection, which occurs before
/// the command runs. Other errors may follow execution and cannot be retried.
pub(super) fn execute_command(
    client: &ApiClient,
    session_id: &str,
    line: &str,
) -> Result<Value, CallError> {
    let result = client.call(
        EXECUTE_METHOD,
        json!({ "agentId": session_id, "line": line, "submittedAttachments": [] }),
    );

    match result {
        Err(CallError::Business { code, message })
            if code == "gateway/arguments-invalid"
                && message
                    == "typert gateway: commands/execute: args fields do not match the descriptor: missing \"images\"; unexpected \"submittedAttachments\"" =>
        {
            client.call(
                EXECUTE_METHOD,
                json!({ "agentId": session_id, "line": line, "images": [] }),
            )
        }
        result => result,
    }
}

/// Read a `skill.list` result.
///
/// A skill is invoked by writing `/name` into an ordinary prompt, which the
/// host recognizes before the step runs; there is no invocation call, so the
/// catalog exists to name what can be written rather than what can be called.
pub(crate) fn skill_catalog(value: &Value) -> SkillCatalog {
    SkillCatalog {
        skills: value["skills"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|skill| {
                let name = skill["name"].as_str()?.to_string();

                let description = match skill["whenToUse"].as_str() {
                    Some(when) if !when.is_empty() => {
                        format!(
                            "{} — {when}",
                            skill["description"].as_str().unwrap_or_default()
                        )
                    }
                    _ => skill["description"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                };

                Some(SkillInfo {
                    // The harness identifies a skill by name alone; the path is
                    // an identity the file-backed harnesses need because they
                    // can publish one name from several directories.
                    path: name.clone(),
                    name,
                    description,
                    // The catalog is resolved from the session's own project
                    // root, so every entry reaches the same distance.
                    scope: "project".to_string(),
                    // The catalog lists what a user can invoke, so anything on
                    // it is available by definition.
                    enabled: true,
                    display_name: None,
                })
            })
            .collect(),
        // The list either arrives or the call fails; there is no partial read
        // that names which entries could not be loaded.
        errors: Vec::new(),
    }
}

/// Read a `commands/execute` result.
///
/// The registry settles a command before answering, so this is the outcome
/// rather than an acknowledgement. A name or a line the registry could not
/// resolve produces no answer at all, which is a refusal the caller has to
/// report itself: nothing ran and nothing will.
pub(crate) fn command_outcome(name: &str, value: &Value) -> SlashCommandOutcome {
    let text = value["result"]["text"].as_str().map(str::to_string);

    match value["result"]["kind"].as_str() {
        Some("success") => SlashCommandOutcome::Completed { message: text },
        Some(_) => SlashCommandOutcome::Rejected {
            message: text.unwrap_or_else(|| format!("/{name} failed")),
        },
        None => SlashCommandOutcome::Rejected {
            message: format!("the harness does not recognize /{name}"),
        },
    }
}

/// Read a `commands/list` result.
pub(crate) fn command_catalog(value: &Value) -> Vec<SlashCommandInfo> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|command| {
            let hint = command["input"]["hint"].as_str();

            Some(SlashCommandInfo {
                name: command["name"].as_str()?.to_string(),
                description: command["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                argument_hint: hint.map(str::to_string),
                source: SlashCommandSource::Provider,
                // A command advertising an input hint takes free text after its
                // name; one without it is the whole line.
                arguments: match hint {
                    Some(_) => SlashCommandArguments::Freeform,
                    None => SlashCommandArguments::None,
                },
                // The registry runs a command itself instead of handing it to
                // the model, so none of them wait for a turn to end.
                run_policy: SlashCommandRunPolicy::Immediate,
            })
        })
        .collect()
}
