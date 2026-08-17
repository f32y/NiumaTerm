//! The child agents a conversation spawned.
//!
//! The harness keeps children as sessions of their own and answers for the
//! direct level only, so a row describes one child rather than a subtree. Both
//! reads are pure functions over a unary result; the session owns the calls.

use serde_json::Value;

use crate::background_task::{
    BackgroundTaskDiscoveryState, BackgroundTaskKey, BackgroundTaskRefs, BackgroundTaskSnapshot,
    BackgroundTaskState, BackgroundTaskSummary,
};
use crate::chat::Item;
use crate::deepseek::history;

/// Read a `subagent.list` result into the snapshot the panel renders.
///
/// A diagnostic row names a child the harness could not read; it is dropped
/// rather than shown, because nothing about it can be opened and a row that
/// only reports its own unreadability is noise beside working children.
pub(crate) fn snapshot(
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
        .filter_map(|entry| {
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
        })
        .collect();

    BackgroundTaskSnapshot {
        parent_session,
        tasks,
        discovery: BackgroundTaskDiscoveryState::Ready,
        activity,
    }
}

/// Read a `subagent.history` page into the child's own conversation.
///
/// A child's page carries the same events and render cards the parent's does,
/// so the rebuild is the parent's, flattened: the panel shows one stream rather
/// than the turn folds the main transcript draws.
pub(crate) fn transcript(value: &Value) -> Vec<Item> {
    history::replay(value)
        .into_iter()
        .flat_map(|turn| turn.items)
        .map(|entry| entry.item)
        .collect()
}
