//! The harness's own slash commands.
//!
//! These reach a different endpoint family than the rest of this adapter: the
//! session methods are served by the host's API proxy, while the command
//! registry is a service behind the typed RPC gateway. Both speak the same
//! request message over the same `/api` route, so only the method name and the
//! named-argument wrapper differ.

use serde_json::{Value, json};

use crate::chat::{
    SkillCatalog, SkillInfo, SlashCommandArguments, SlashCommandInfo, SlashCommandOutcome,
    SlashCommandRunPolicy, SlashCommandSource,
};

/// Gateway endpoint listing one session's effective commands.
pub(crate) const LIST_METHOD: &str = "commands/list";

/// Gateway endpoint running one command line.
///
/// Sending the line as an ordinary prompt does not run it: the host admits a
/// prompt to the agent whatever it starts with, so a slash line delivered that
/// way reaches the model as text.
pub(crate) const EXECUTE_METHOD: &str = "commands/execute";

/// Named arguments for a gateway call addressed to one session's agent.
///
/// The registry resolves the agent from a session id, and the argument is named
/// by the resolver that does the resolving rather than by the method's own
/// parameter, so the two names differ.
pub(crate) fn agent_args(session_id: &str) -> Value {
    json!({ "args": { "agentId": session_id } })
}

/// How the gateway refuses an argument set carrying an image list on a release
/// whose command descriptor has no image parameter. The check compares the
/// whole field set, so an empty list is as unacceptable there as a full one,
/// and the refusal is what tells this apart from a command that failed.
pub(crate) const UNEXPECTED_IMAGES: &str = "unexpected \"images\"";

/// Named arguments for one command line addressed to a session's agent.
///
/// From 0.1.1 a command can be given attachments, and the field is required
/// whether or not the command reads them. The commands this adapter runs carry
/// none, so the list is present and empty.
pub(crate) fn execute_args(session_id: &str, line: &str) -> Value {
    json!({ "args": { "agentId": session_id, "line": line, "images": [] } })
}

/// The same call for a release that predates the image parameter.
pub(crate) fn execute_args_without_images(session_id: &str, line: &str) -> Value {
    json!({ "args": { "agentId": session_id, "line": line } })
}

/// Read a `skill.list` result.
///
/// A skill is invoked by writing `/name` into an ordinary prompt, which the
/// host recognizes before the step runs; there is no invocation call, so the
/// catalog exists to name what can be written rather than what can be called.
pub(crate) fn skills(value: &Value) -> SkillCatalog {
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
pub(crate) fn outcome(name: &str, value: &Value) -> SlashCommandOutcome {
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
pub(crate) fn catalog(value: &Value) -> Vec<SlashCommandInfo> {
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
