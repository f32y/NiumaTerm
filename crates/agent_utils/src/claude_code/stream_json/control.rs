use std::collections::HashMap;
use std::mem::take;

use serde_json::Value;

use crate::chat::Event;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PendingControlOperation {
    FileRewind,
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
    }
}

pub(super) fn fail_pending_control_operations(
    pending: &mut HashMap<String, PendingControlOperation>,
    message: &str,
) -> Vec<Event> {
    let operations = take(pending);

    operations
        .into_values()
        .map(|operation| match operation {
            PendingControlOperation::FileRewind => Event::FileRewindCompleted {
                error: Some(message.to_string()),
            },
        })
        .collect()
}
