//! Reading persisted conversations back from the host.
//!
//! Both halves are pure readers over a unary result: the session list an empty
//! tab offers, and one session's own events rebuilt into the turns the pane
//! replays. The events are the same ones the live stream carries, so the
//! rebuild reuses the live mapping rather than describing the vocabulary twice.

use std::collections::HashMap;
use std::mem::take;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::Value;

use crate::chat::{
    Event, ForkAnchor, ForkCheckpoint, Item, ReplayItem, ReplayTurn, SessionSummary,
};
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
                // The harness records neither a branch nor a working
                // directory on a session.
                branch: None,
                cwd: None,
                last_active: item["updatedAt"]
                    .as_u64()
                    .map(|millis| UNIX_EPOCH + Duration::from_millis(millis))
                    .unwrap_or(UNIX_EPOCH),
                snippet: None,
            })
        })
        .collect()
}

/// Read a `session.search` result into the same rows the recent list uses.
///
/// The search answers with matching session ids and an excerpt each, and
/// nothing else: titles, timestamps, and the working directory a row is
/// filtered by all belong to the list. So the two are read together and joined
/// here, which also applies the list's own exclusions to the results — a
/// subagent's session or one rooted elsewhere is no more resumable for having
/// matched a query.
pub(crate) fn search_results(
    matches: &Value,
    listed: &Value,
    cwd: Option<&str>,
) -> Vec<SessionSummary> {
    let excerpts: HashMap<&str, &str> = matches["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| Some((item["sessionId"].as_str()?, item["snippet"].as_str()?)))
        .collect();

    // The search ranks its answers and the list is ordered by recency, so the
    // rows are emitted in the search's order rather than the list's.
    let rows = sessions(listed, cwd);
    matches["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["sessionId"].as_str())
        .filter_map(|id| {
            let mut row = rows.iter().find(|row| row.id == id)?.clone();
            row.snippet = excerpts.get(id).map(|snippet| snippet.to_string());
            Some(row)
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

/// Read the prompts a branch of this conversation can be cut in front of out
/// of a history page, newest first.
///
/// Cutting in front of a prompt keeps every turn before the one that prompt
/// opened. The harness anchors such a cut on an event seq and extends it to
/// the end of the turn that seq falls in, so each prompt is paired with the
/// seq of the prompt ahead of it. The oldest prompt on the page has no prompt
/// ahead of it: on a page that reaches the start of the log, branching in
/// front of it is an empty conversation, which starting a new one already is,
/// and on a page that does not, its predecessor is simply not loaded.
pub(crate) fn fork_checkpoints(page: &Value) -> Vec<ForkCheckpoint> {
    let prompts: Vec<(u64, String, Option<u64>)> = page["events"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| &entry["event"])
        .filter(|event| event["type"].as_str() == Some("user/message"))
        .filter_map(|event| {
            // The log records more than typed prompts under this type, and a
            // branch is offered in front of what the person actually asked.
            // The live mapping decides what counts as a typed prompt, so the
            // rule stays in one place: the log records more than the person's
            // own messages under this type. A prompt maps to exactly one item,
            // and anything else the mapper produced is not one.
            let mut mapped = map_session_event(event, &Value::Null, &mut ToolTracker::default());
            let Some(Event::ItemStarted(Item::UserMessage { text: Some(text) })) = mapped.pop()
            else {
                return None;
            };
            Some((event["seq"].as_u64()?, text, event["time"].as_u64()))
        })
        .collect();

    let mut checkpoints: Vec<ForkCheckpoint> = prompts
        .windows(2)
        .filter_map(|pair| {
            let [(kept, _, _), (_, prompt, at)] = pair else {
                return None;
            };
            Some(ForkCheckpoint {
                prompt: prompt.clone(),
                timestamp: at.map(unix_millis_to_rfc3339),
                anchor: ForkAnchor::DeepSeekThrough(*kept),
            })
        })
        .collect();

    checkpoints.reverse();
    checkpoints
}

/// The harness dates its events in Unix milliseconds while the picker renders
/// RFC 3339, which is what a backend reading its history off disk records.
fn unix_millis_to_rfc3339(millis: u64) -> String {
    chrono::DateTime::from_timestamp_millis(millis as i64)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
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
