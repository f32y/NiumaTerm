//! The harness's own slash commands.
//!
//! These reach a different endpoint family than the rest of this adapter: the
//! session methods are served by the host's API proxy, while the command
//! registry is a service behind the typed RPC gateway. Both speak the same
//! request message over the same `/api` route, so only the method name and the
//! named-argument wrapper differ.

use serde_json::{Value, json};

use crate::chat::{
    SlashCommandArguments, SlashCommandInfo, SlashCommandRunPolicy, SlashCommandSource,
};

/// Gateway endpoint listing one session's effective commands.
pub(crate) const LIST_METHOD: &str = "commands/list";

/// Named arguments for a gateway call addressed to one session's agent.
///
/// The registry resolves the agent from a session id, and the argument is named
/// by the resolver that does the resolving rather than by the method's own
/// parameter, so the two names differ.
pub(crate) fn agent_args(session_id: &str) -> Value {
    json!({ "args": { "agentId": session_id } })
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
