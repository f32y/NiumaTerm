use std::collections::HashMap;
use std::mem::take;

use serde_json::Value;

use crate::chat::{ContextComposition, ContextSegment, Event};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PendingControlOperation {
    FileRewind,
    ContextComposition,
}

/// A `can_use_tool` control request awaiting the user's decision. The original
/// input is kept because an allow response must echo it as `updatedInput`, and
/// the CLI's permission suggestions back the "always allow" decision.
pub(super) struct PendingApproval {
    pub(super) request_id: String,
    pub(super) input: Value,
    pub(super) suggestions: Option<Value>,
}

pub(super) fn resolve_pending_control_operation(
    pending: &mut HashMap<String, PendingControlOperation>,
    response: &Value,
) -> Option<Event> {
    let request_id = response["request_id"].as_str()?;
    let operation = pending.remove(request_id)?;
    let error = match response["subtype"].as_str() {
        Some("success") => None,
        Some("error") => Some(
            response["error"]
                .as_str()
                .unwrap_or("unknown Claude control error")
                .to_string(),
        ),
        _ => Some("Claude returned a malformed file restore response.".to_string()),
    };

    match operation {
        PendingControlOperation::FileRewind => Some(Event::FileRewindCompleted { error }),
        // A composition that could not be computed leaves the previous
        // breakdown in place: the accounting beside it is still accurate, and
        // an error here says nothing about the conversation.
        PendingControlOperation::ContextComposition => error
            .is_none()
            .then(|| parse_context_composition(&response["response"]))
            .flatten()
            .map(Event::ContextCompositionUpdated),
    }
}

/// Read the CLI's context breakdown. Every field beyond the segments is
/// optional because a payload that drops one still describes the split
/// usefully, and the alternative is showing nothing.
fn parse_context_composition(payload: &Value) -> Option<ContextComposition> {
    let segments: Vec<ContextSegment> = payload["categories"]
        .as_array()?
        .iter()
        .filter_map(|category| {
            let tokens = category["tokens"].as_u64()?;
            Some(ContextSegment {
                label: category["name"].as_str()?.to_owned(),
                tokens,
                color: category["color"].as_str().map(str::to_owned),
                deferred: category["isDeferred"].as_bool().unwrap_or(false),
            })
        })
        .collect();

    if segments.is_empty() {
        return None;
    }

    Some(ContextComposition {
        used_tokens: payload["totalTokens"]
            .as_u64()
            .unwrap_or_else(|| segments.iter().map(|segment| segment.tokens).sum()),
        max_tokens: payload["maxTokens"].as_u64().filter(|max| *max > 0),
        raw_max_tokens: payload["rawMaxTokens"].as_u64().filter(|max| *max > 0),
        auto_compact_threshold: payload["autoCompactThreshold"]
            .as_u64()
            .filter(|threshold| *threshold > 0),
        segments,
    })
}

pub(super) fn fail_pending_control_operations(
    pending: &mut HashMap<String, PendingControlOperation>,
    message: &str,
) -> Vec<Event> {
    let operations = take(pending);

    operations
        .into_values()
        .filter_map(|operation| match operation {
            PendingControlOperation::FileRewind => Some(Event::FileRewindCompleted {
                error: Some(message.to_string()),
            }),
            // Nothing is waiting on a breakdown, so a lost one is not worth
            // reporting; the next turn asks again.
            PendingControlOperation::ContextComposition => None,
        })
        .collect()
}
