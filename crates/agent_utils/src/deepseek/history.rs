//! Reading persisted conversations back from the host.
//!
//! Both halves are pure readers over a unary result: the session list an empty
//! tab offers, and one session's own events rebuilt into the turns the pane
//! replays. The events are the same ones the live stream carries, so the
//! rebuild reuses the live mapping rather than describing the vocabulary twice.

use std::mem::take;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::Value;

use crate::chat::{Event, Item, ReplayItem, ReplayTurn, SessionSummary};
use crate::deepseek::mapping::{ToolTracker, map_session_event};

/// Read a `session.list` result into the resumable conversations of one
/// working directory.
///
/// A tab works in one project, so a session rooted elsewhere is not something
/// it can continue. Sessions the harness reports as blank never ran a turn and
/// have nothing to resume into, and a subagent's session belongs to its parent
/// rather than to this list.
pub(crate) fn sessions(value: &Value, cwd: Option<&str>) -> Vec<SessionSummary> {
    value["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["blank"] != Value::Bool(true))
        .filter(|item| item["origin"].as_str() != Some("subagent"))
        .filter(|item| match cwd {
            Some(cwd) => item["cwd"].as_str() == Some(cwd),
            None => true,
        })
        .filter_map(|item| {
            let id = item["sessionId"].as_str()?.to_string();
            // The title rides the projection baseline the list row carries. A
            // session too new to have been titled shows its own id, which is
            // still what picking it will open.
            let title = item["projections"]["values"]["title"]
                .as_str()
                .filter(|title| !title.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| id.clone());

            Some(SessionSummary {
                title,
                id,
                // The harness records no branch on a session.
                branch: None,
                last_active: item["updatedAt"]
                    .as_u64()
                    .map(|millis| UNIX_EPOCH + Duration::from_millis(millis))
                    .unwrap_or(UNIX_EPOCH),
            })
        })
        .collect()
}

/// Rebuild one session's turns from a `session.history` page.
///
/// The page carries the raw events plus the host's render cards, which is what
/// lets the live mapping do the work: a replayed tool call produces the same
/// row it produced when it ran. A turn's accounting comes from the boundary
/// events themselves, because the item stream cannot express how long a turn
/// took or whether the user stopped it.
pub(crate) fn replay(value: &Value) -> Vec<ReplayTurn> {
    let mut tools = ToolTracker::default();
    let mut turns: Vec<ReplayTurn> = Vec::new();
    let mut current = ReplayTurn::default();
    let mut started_at: Option<u64> = None;

    for entry in value["events"].as_array().into_iter().flatten() {
        let event = &entry["event"];
        let time = event["time"].as_u64();

        match event["type"].as_str() {
            Some("turn/start") => {
                // A page can begin mid-turn, and those items belong to a turn
                // whose start is on an older page rather than to this one.
                if !current.items.is_empty() {
                    turns.push(take(&mut current));
                }
                started_at = time;
            }
            Some("turn/end") => {
                current.interrupted = event["data"]["reason"]["kind"].as_str() == Some("aborted");
                current.seconds = started_at
                    .zip(time)
                    .map(|(start, end)| end.saturating_sub(start) / 1000);
                turns.push(take(&mut current));
                started_at = None;
                continue;
            }
            _ => {}
        }

        for mapped in map_session_event(event, &entry["view"], &mut tools) {
            match mapped {
                Event::ItemStarted(item) => current.items.push(ReplayItem {
                    item,
                    at: time.map(|millis| (millis / 1000) as i64),
                }),
                // A completed payload finishes the row its streamed half
                // opened; only an item with no such half is a row of its own.
                Event::ItemCompleted(item) => {
                    let merged = current
                        .items
                        .iter_mut()
                        .rev()
                        .any(|existing| existing.item.merge_completed(&item));
                    if !merged {
                        current.items.push(ReplayItem {
                            item,
                            at: time.map(|millis| (millis / 1000) as i64),
                        });
                    }
                }
                // Turn boundaries are read from the raw events above, and the
                // rest of the vocabulary describes live state a replay has no
                // moment to apply it to.
                _ => {}
            }
        }
    }

    if !current.items.is_empty() {
        turns.push(current);
    }

    turns
}

/// Flatten a rebuilt page into one stream of items.
///
/// A panel that shows a child's conversation beside its parent's has no room
/// for turn folds and no second turn counter to hang them from, so the turns
/// are read for their contents alone.
pub(crate) fn items(value: &Value) -> Vec<Item> {
    replay(value)
        .into_iter()
        .flat_map(|turn| turn.items)
        .map(|entry| entry.item)
        .collect()
}
