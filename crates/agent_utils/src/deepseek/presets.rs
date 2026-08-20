//! The agent compositions a deployment offers.
//!
//! A preset names the plugins a conversation's agent is built from, so it
//! decides what tools that conversation can ever call. The roster belongs to
//! the deployment rather than to this application, which is why it is read
//! rather than written here.

use serde_json::Value;

use crate::chat::AgentPreset;

/// Read an `agentPreset.list` result.
///
/// A preset that cannot compose a session stays on the harness's own roster —
/// its directory still occupies the id, and the harness's authoring surface has
/// to be able to show and delete it — but this picker only selects, so offering
/// one here would trade a visible reason now for a failed conversation later.
pub(crate) fn catalog(value: &Value) -> Vec<AgentPreset> {
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
