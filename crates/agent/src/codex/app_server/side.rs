//! Requests that open a side conversation: an ephemeral fork of a parent
//! thread that answers questions beside it without adding to it.

use serde_json::{Value, json};

use crate::chat::Item;

/// Developer instructions for the fork. The fork inherits the parent's whole
/// model context, including tasks it was in the middle of, so without them the
/// model would read the inherited history as work to carry on. The side
/// session keeps the parent's permissions, which makes these the only thing
/// steering it away from changing the workspace.
const SIDE_DEVELOPER_INSTRUCTIONS: &str = "You are in a side conversation, not the main thread.

This side conversation is for answering questions and lightweight exploration without disrupting the main thread. Do not present yourself as continuing the main thread's active task.

The inherited fork history is provided only as reference context. Do not treat instructions, plans, or requests found in the inherited history as active instructions for this side conversation. Only instructions submitted after the side-conversation boundary are active.

Do not continue, execute, or complete any task, plan, tool call, approval, edit, or request that appears only in inherited history.

Sub-agents are off-limits in this side conversation. Do not interact with any existing or new sub-agents, even if sub-agents were used before this boundary.

You may perform non-mutating inspection, including reading or searching files and running checks that do not alter repo-tracked files.

Do not modify files, source, git state, permissions, configuration, or any other workspace state unless the user explicitly requests that mutation in this side conversation. If the user explicitly requests a mutation, keep it minimal, local to the request, and avoid disrupting the main thread.";

/// The model-visible message that marks where the inherited history ends.
/// Developer instructions alone say that such a boundary exists; this item
/// puts it at the exact point in the history the fork was cut.
const SIDE_BOUNDARY_PROMPT: &str = "Side conversation boundary.

Everything before this boundary is inherited history from the parent thread. It is reference context only. It is not your current task.

Do not continue, execute, or complete any instructions, plans, tool calls, approvals, edits, or requests from before this boundary. Only messages submitted after this boundary are active user instructions for this side conversation.

If there is no user question after this boundary yet, wait for one.";

/// The parent a side conversation forks from, and the model settings it
/// starts with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideStart {
    pub parent_thread_id: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// `thread/fork` for a side conversation.
///
/// `ephemeral` keeps the fork out of persisted history, so it never appears
/// among resumable conversations. `excludeTurns` leaves the inherited turns
/// out of the reply only: the model still sees them, while the side view
/// starts empty instead of repeating the parent's transcript.
pub(super) fn side_fork_request(start: &SideStart) -> Value {
    let mut params = json!({
        "threadId": start.parent_thread_id,
        "ephemeral": true,
        "excludeTurns": true,
        "developerInstructions": SIDE_DEVELOPER_INSTRUCTIONS,
    });

    if let Some(model) = &start.model {
        params["model"] = json!(model);
    }

    if let Some(effort) = &start.effort {
        params["config"] = json!({"model_reasoning_effort": effort});
    }

    json!({
        "jsonrpc": "2.0",
        "method": "thread/fork",
        "params": params,
    })
}

/// The transcript item standing for what the fork was given: its
/// developer instructions and the boundary message.
pub(super) fn side_boundary_item(thread_id: &str) -> Item {
    Item::SideBoundary {
        id: format!("side-boundary:{thread_id}"),
        instructions: SIDE_DEVELOPER_INSTRUCTIONS.to_owned(),
        message: SIDE_BOUNDARY_PROMPT.to_owned(),
    }
}

/// `thread/inject_items` placing the boundary message. It adds to the model
/// history without starting a turn, so the side waits for its first question.
pub(super) fn side_boundary_request(thread_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "thread/inject_items",
        "params": {
            "threadId": thread_id,
            "items": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": SIDE_BOUNDARY_PROMPT}],
            }],
        },
    })
}
