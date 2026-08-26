//! Remote cleanup for a tab that is releasing its Harness session.

use std::thread;
use std::time::Duration;

use serde_json::json;

use crate::deepseek::api::ApiClient;

/// Close cleanup is detached from the UI thread and should give up quickly if
/// another tab no longer keeps the shared host reachable.
const CLOSE_CALL_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CloseAction {
    RemoveQueued(String),
    CancelTurn,
}

/// Apply the remote work needed before a tab forgets its session. Queue entries
/// are removed before the active turn is cancelled because the Harness keeps
/// its inbox on cancellation and would otherwise start another invisible turn.
pub(crate) fn run_close_actions(
    client: &ApiClient,
    session_id: &str,
    actions: &[CloseAction],
) -> Vec<String> {
    let mut failures = Vec::new();

    for action in actions {
        let (method, payload) = match action {
            CloseAction::RemoveQueued(item_id) => (
                "session.updateQueue",
                json!({
                    "sessionId": session_id,
                    "itemId": item_id,
                    "action": { "kind": "remove" },
                }),
            ),
            CloseAction::CancelTurn => ("session.cancel", json!({ "sessionId": session_id })),
        };

        if let Err(error) = client.call_with_timeout(method, payload, CLOSE_CALL_TIMEOUT) {
            failures.push(format!("{method}: {}", error.message()));
        }
    }

    failures
}

pub(crate) fn schedule_close_actions(
    client: ApiClient,
    session_id: String,
    actions: Vec<CloseAction>,
) {
    if actions.is_empty() {
        return;
    }

    let spawn = thread::Builder::new()
        .name("deepseek-close".to_string())
        .spawn(move || {
            for failure in run_close_actions(&client, &session_id, &actions) {
                tracing::warn!("deepseek session close cleanup failed: {failure}");
            }
        });
    if let Err(error) = spawn {
        tracing::warn!("deepseek session close cleanup could not start: {error}");
    }
}
