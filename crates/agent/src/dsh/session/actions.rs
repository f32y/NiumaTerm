//! User commands and interaction answers for a session.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::chat::MessageImage;
use crate::dsh::api::ApiClient;
use crate::dsh::catalogs;

/// Image bytes travel inline rather than by reference: the harness's
/// attachment method reads what a conversation already holds and is no route
/// for putting something into one. A model that declines image input refuses
/// the whole prompt, which is a business error the transcript reports, so
/// nothing is dropped silently to make a message fit.
pub(super) fn prompt_payload(
    session_id: &str,
    text: &str,
    mode: &str,
    images: &[MessageImage],
) -> Value {
    let mut content = vec![json!({ "type": "text", "text": text })];

    content.extend(images.iter().map(|image| {
        json!({
            "type": "image",
            "mediaType": image.media_type,
            "data": BASE64_STANDARD.encode(&image.bytes),
        })
    }));

    json!({
        "requestId": Uuid::new_v4().to_string(),
        "sessionId": session_id,
        "mode": mode,
        "content": content,
    })
}

/// Run one command line against the session's registry and describe the
/// harness's answer as the settled command a frame carries.
pub(super) async fn run_slash(
    client: &ApiClient,
    session_id: &str,
    name: &str,
    arguments: &str,
) -> Value {
    let line = match arguments.trim() {
        "" => format!("/{name}"),
        arguments => format!("/{name} {arguments}"),
    };

    let mut command = json!({ "kind": "slash", "name": name, "arguments": arguments });

    match catalogs::execute_command(client, session_id, &line).await {
        Ok(value) => command["value"] = value,
        Err(error) => command["error"] = json!(error.message()),
    }

    command
}

// Remote cleanup for a tab that is releasing its Harness session.

/// Close cleanup is detached from the closing thread and should give up quickly if
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
pub(crate) async fn run_close_actions(
    client: &ApiClient,
    session_id: &str,
    actions: &[CloseAction],
) -> Vec<String> {
    let mut failures = Vec::new();

    for action in actions {
        let (method, payload) = match action {
            CloseAction::RemoveQueued(item_id) => (
                "session/updateQueue",
                json!({
                    "sessionId": session_id,
                    "itemId": item_id,
                    "action": { "kind": "remove" },
                }),
            ),
            CloseAction::CancelTurn => ("session/cancel", json!({ "sessionId": session_id })),
        };

        if let Err(error) = client
            .call_with_timeout(method, json!({ "request": payload }), CLOSE_CALL_TIMEOUT)
            .await
        {
            failures.push(format!("{method}: {}", error.message()));
        }
    }

    failures
}

pub(super) fn schedule_close_actions(
    client: ApiClient,
    session_id: String,
    actions: Vec<CloseAction>,
) {
    if actions.is_empty() {
        return;
    }

    nmt_platform::runtime().spawn(async move {
        for failure in run_close_actions(&client, &session_id, &actions).await {
            tracing::warn!("deepseek session close cleanup failed: {failure}");
        }
    });
}
