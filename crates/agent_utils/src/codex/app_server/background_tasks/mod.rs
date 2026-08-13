//! Codex descendant-thread tracking for the `Background Tasks` view.
//!
//! Codex reports child agents as separate threads on the same app-server
//! process. Their turn, item, and status notifications arrive on the same
//! stream as the parent's, so this module owns the thread-id routing that keeps
//! child lifecycle out of the parent conversation, plus the `thread/list`
//! descendant query used to recover children after a resume or reconnect.

mod launch_messages;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::background_task::{
    BackgroundTaskDiscoveryState, BackgroundTaskKey, BackgroundTaskRefs, BackgroundTaskRegistry,
    BackgroundTaskSnapshot, BackgroundTaskState, BackgroundTaskUpdate,
};
use crate::chat::Item;
use crate::codex::app_server::background_tasks::launch_messages::LaunchMessages;

/// Page size for descendant discovery. Threads are cheap metadata rows and the
/// cursor is followed to completion, so this only bounds one response.
const DESCENDANT_PAGE_LIMIT: u64 = 50;

/// Updates held for thread ids that have not been confirmed as descendants.
/// Unrelated threads on the same process would otherwise accumulate forever, so
/// the oldest candidate is dropped once this many are waiting.
const MAX_PENDING_THREADS: usize = 64;

/// Where a notification's thread id points relative to the selected parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ThreadScope {
    /// The selected parent thread; existing parent handling applies.
    Parent,
    /// A thread confirmed to descend from the parent.
    Descendant,
    /// Some other thread id, or one whose relationship is not yet known.
    Unrelated,
    /// The notification is not scoped to a thread at all.
    Unscoped,
}

/// One in-flight `thread/list` descendant page.
#[derive(Clone, Copy, Debug)]
struct DescendantQuery {
    /// Registry order when the request was written. A row that changed after
    /// this point keeps its live state instead of taking the queried one.
    starting_sequence: u64,
}

#[derive(Default)]
pub(super) struct CodexTasks {
    registry: Option<BackgroundTaskRegistry>,
    /// Thread ids proven to descend from the selected root, either by a
    /// collaboration item or by a descendant query row.
    confirmed: HashSet<String>,
    /// Immediate parent of each confirmed descendant, used to validate that a
    /// later row still reaches the selected root.
    parents: HashMap<String, String>,
    /// Latest state seen for a thread whose relationship is still unknown.
    /// Only the newest candidate per thread is kept: a child update can arrive
    /// before its spawn item, but unrelated thread content must never reach the
    /// parent conversation.
    pending: HashMap<String, BackgroundTaskUpdate>,
    /// Insertion order of `pending`, so the oldest candidate can be evicted.
    pending_order: Vec<String>,
    launch_messages: LaunchMessages,
    queries: HashMap<u64, DescendantQuery>,
    /// Pagination cursors already requested for the current root.
    seen_cursors: HashSet<String>,
    /// In-flight `thread/read` requests, by the descendant they will deliver.
    /// One per child at a time: a second read would return the same stored
    /// conversation and only cost another round trip.
    reads: HashMap<u64, String>,
}

impl CodexTasks {
    /// Point the registry at a parent thread. Returns true when this is a
    /// different root, which is what makes the caller start descendant
    /// discovery and drop rows belonging to the previous conversation.
    pub(super) fn set_root(&mut self, thread_id: &str) -> bool {
        if self.root() == Some(thread_id) {
            return false;
        }
        self.registry = Some(BackgroundTaskRegistry::new(BackgroundTaskKey::codex(
            thread_id,
        )));
        self.confirmed.clear();
        self.parents.clear();
        self.pending.clear();
        self.pending_order.clear();
        self.launch_messages.clear();
        self.queries.clear();
        self.seen_cursors.clear();
        self.reads.clear();
        true
    }

    /// Whether a returned cursor is worth following. A cursor already used for
    /// this root means the server is repeating a page.
    pub(super) fn accept_cursor(&mut self, cursor: &str) -> bool {
        self.seen_cursors.insert(cursor.to_owned())
    }

    pub(super) fn root(&self) -> Option<&str> {
        self.registry
            .as_ref()
            .map(|registry| registry.parent_session().id.as_str())
    }

    pub(super) fn snapshot(&self) -> Option<BackgroundTaskSnapshot> {
        self.registry.as_ref().map(BackgroundTaskRegistry::snapshot)
    }

    /// Classify a thread id against the selected root. Before a root is known
    /// there is nothing to route against, so every notification keeps its
    /// existing parent handling.
    pub(super) fn scope(&self, thread_id: Option<&str>) -> ThreadScope {
        let (Some(thread_id), Some(root)) = (thread_id, self.root()) else {
            return ThreadScope::Unscoped;
        };
        if root == thread_id {
            return ThreadScope::Parent;
        }
        if self.confirmed.contains(thread_id) {
            return ThreadScope::Descendant;
        }
        ThreadScope::Unrelated
    }

    /// Record a child relationship and report whether the thread is a valid
    /// descendant of the selected root. `parent_thread_id` may be another
    /// descendant; the chain is walked so a row that never reaches the selected
    /// root — or that closes a cycle — is refused.
    fn confirm(&mut self, thread_id: &str, parent_thread_id: Option<&str>) -> bool {
        let Some(root) = self.root().map(str::to_owned) else {
            return false;
        };
        if thread_id == root {
            return false;
        }
        if let Some(parent) = parent_thread_id {
            if parent != root && !self.confirmed.contains(parent) {
                return false;
            }
            if !self.chain_reaches_root(parent, &root) {
                return false;
            }
            self.parents.insert(thread_id.to_owned(), parent.to_owned());
        }
        self.confirmed.insert(thread_id.to_owned());
        self.launch_messages.confirm(thread_id);
        true
    }

    /// Follow immediate-parent links up to the selected root. A missing link or
    /// a repeated id ends the walk, so malformed data cannot loop forever.
    fn chain_reaches_root(&self, start: &str, root: &str) -> bool {
        let mut seen = HashSet::new();
        let mut current = start;
        loop {
            if current == root {
                return true;
            }
            if !seen.insert(current.to_owned()) {
                return false;
            }
            match self.parents.get(current) {
                Some(parent) => current = parent.as_str(),
                // A confirmed descendant with no recorded parent was proven by
                // a collaboration item on the root's own stream.
                None => return self.confirmed.contains(current),
            }
        }
    }

    /// Depth of a confirmed descendant below the selected root; direct children
    /// are depth 1. `None` when the chain is not fully known yet.
    fn depth_of(&self, thread_id: &str) -> Option<u32> {
        let root = self.root()?;
        let mut seen = HashSet::new();
        let mut current = thread_id.to_owned();
        let mut depth = 1;
        loop {
            let Some(parent) = self.parents.get(&current) else {
                return None;
            };
            if parent == root {
                return Some(depth);
            }
            if !seen.insert(current.clone()) {
                return None;
            }
            current = parent.clone();
            depth += 1;
        }
    }

    /// Apply an update for a thread whose relationship to the root is known, or
    /// hold it as the newest candidate when it is not.
    fn record(
        &mut self,
        thread_id: &str,
        update: BackgroundTaskUpdate,
        confirmed_now: bool,
    ) -> bool {
        if !confirmed_now && !self.confirmed.contains(thread_id) {
            self.hold_pending(thread_id, update);
            return false;
        }
        let depth = self.depth_of(thread_id);
        let Some(registry) = self.registry.as_mut() else {
            return false;
        };
        let update = BackgroundTaskUpdate {
            depth: update.depth.or(depth),
            ..update
        };
        registry.apply(BackgroundTaskKey::codex(thread_id), update)
    }

    fn hold_pending(&mut self, thread_id: &str, update: BackgroundTaskUpdate) {
        if !self.pending.contains_key(thread_id) {
            if self.pending_order.len() >= MAX_PENDING_THREADS {
                let oldest = self.pending_order.remove(0);
                self.pending.remove(&oldest);
            }
            self.pending_order.push(thread_id.to_owned());
        }
        self.pending.insert(thread_id.to_owned(), update);
    }

    /// Move a held candidate into the registry once its relationship is proven.
    fn drain_pending(&mut self, thread_id: &str) -> bool {
        let Some(update) = self.pending.remove(thread_id) else {
            return false;
        };
        self.pending_order.retain(|held| held != thread_id);
        self.record(thread_id, update, true)
    }

    /// Read a collaboration item published on the parent's stream. Returns true
    /// when task state changed. The item is still parsed into the parent
    /// transcript by the caller, because it is the parent's own tool call.
    pub(super) fn observe_parent_item(&mut self, item: &Value) -> bool {
        match item["type"].as_str() {
            Some("collabAgentToolCall") => self.observe_collab_tool_call(item),
            Some("subAgentActivity") => self.observe_subagent_activity(item),
            _ => false,
        }
    }

    /// `collabAgentToolCall` is the parent's own spawn/send/wait/close call.
    /// Its `receiverThreadIds` name the children it targets and `agentsStates`
    /// carries each child's authoritative lifecycle status, so this is the
    /// primary live source for both identity and state.
    fn observe_collab_tool_call(&mut self, item: &Value) -> bool {
        // The sender is the thread that issued the call: the selected root, or
        // a descendant of it when a child spawns its own child.
        let sender = item["senderThreadId"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| self.root().map(str::to_owned));
        let is_spawn = item["tool"].as_str() == Some("spawnAgent");
        let prompt = text_field(item, &["prompt"]);
        let model = text_field(item, &["model"]);
        let states = &item["agentsStates"];

        let mut receivers: Vec<String> = item["receiverThreadIds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        // A tool call can report a state for a child it does not list as a
        // receiver, so both sources contribute identities.
        for id in states.as_object().into_iter().flatten().map(|(id, _)| id) {
            if !receivers.iter().any(|known| known == id) {
                receivers.push(id.clone());
            }
        }

        let mut changed = false;
        for thread_id in receivers {
            if !self.confirm(&thread_id, sender.as_deref()) {
                continue;
            }
            changed |= self.drain_pending(&thread_id);
            if is_spawn && let Some(prompt) = prompt.as_deref() {
                self.launch_messages.remember(&thread_id, prompt);
            }

            let state = &states[thread_id.as_str()];
            let mut update = BackgroundTaskUpdate {
                refs: Some(BackgroundTaskRefs::Codex {
                    thread_id: thread_id.clone(),
                    parent_thread_id: sender.clone(),
                }),
                state: collab_agent_state(state),
                // `message` carries the child's completion summary or its error
                // text, which is the most useful one-line status available.
                status: text_field(state, &["message"]),
                model: model.clone(),
                updated_at: Some(SystemTime::now()),
                ..BackgroundTaskUpdate::default()
            };
            if is_spawn {
                // Only a spawn's prompt describes what the child was asked to
                // do; a `sendInput` prompt is a later message to it.
                update.objective = prompt.clone();
                update.started_at = Some(SystemTime::now());
            } else {
                update.status = update.status.clone().or_else(|| prompt.clone());
            }
            if update.state.is_some_and(BackgroundTaskState::is_terminal) {
                update.completed_at = Some(SystemTime::now());
            }

            changed |= self.record(&thread_id, update, true);
        }
        changed
    }

    /// `subAgentActivity` reports that a known child started, was interacted
    /// with, or was interrupted. It carries no status text, so it only moves
    /// the lifecycle.
    fn observe_subagent_activity(&mut self, item: &Value) -> bool {
        let Some(thread_id) = item["agentThreadId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
        else {
            return false;
        };
        if !self.confirm(&thread_id, self.root().map(str::to_owned).as_deref()) {
            return false;
        }
        let mut changed = self.drain_pending(&thread_id);

        let state = match item["kind"].as_str() {
            Some("started") => Some(BackgroundTaskState::Working),
            Some("interrupted") => Some(BackgroundTaskState::Interrupted),
            // `interacted` marks input the parent sent; the child's own state
            // has not moved.
            _ => None,
        };
        let update = BackgroundTaskUpdate {
            refs: Some(BackgroundTaskRefs::Codex {
                thread_id: thread_id.clone(),
                parent_thread_id: self.root().map(str::to_owned),
            }),
            state,
            started_at: (state == Some(BackgroundTaskState::Working)).then(SystemTime::now),
            completed_at: state
                .is_some_and(BackgroundTaskState::is_terminal)
                .then(SystemTime::now),
            updated_at: Some(SystemTime::now()),
            ..BackgroundTaskUpdate::default()
        };

        changed |= self.record(&thread_id, update, true);
        changed
    }

    /// Fold a notification that belongs to a confirmed descendant. The parent's
    /// turn id, running state, approval state, and transcript are untouched.
    pub(super) fn apply_descendant_notification(
        &mut self,
        thread_id: &str,
        method: &str,
        params: &Value,
    ) -> bool {
        let update = match method {
            "turn/started" => BackgroundTaskUpdate {
                state: Some(BackgroundTaskState::Working),
                started_at: Some(SystemTime::now()),
                updated_at: Some(SystemTime::now()),
                ..BackgroundTaskUpdate::default()
            },
            "turn/completed" => {
                let state = match params["turn"]["status"].as_str() {
                    Some("failed") => BackgroundTaskState::Failed,
                    Some("interrupted") => BackgroundTaskState::Interrupted,
                    _ => BackgroundTaskState::Done,
                };
                BackgroundTaskUpdate {
                    state: Some(state),
                    completed_at: Some(SystemTime::now()),
                    updated_at: Some(SystemTime::now()),
                    status: params["turn"]["error"]["message"]
                        .as_str()
                        .map(str::to_owned),
                    ..BackgroundTaskUpdate::default()
                }
            }
            "thread/status/changed" => {
                let status = &params["status"];
                let Some(state) = thread_status_state(status) else {
                    return false;
                };
                BackgroundTaskUpdate {
                    state: Some(state),
                    updated_at: Some(SystemTime::now()),
                    completed_at: state.is_terminal().then(SystemTime::now),
                    ..BackgroundTaskUpdate::default()
                }
            }
            "item/started" | "item/completed" => {
                // Child transcript content stays out of the parent conversation;
                // only the row's latest-status preview reflects it.
                let item = &params["item"];
                BackgroundTaskUpdate {
                    last_preview: item_preview(item),
                    updated_at: Some(SystemTime::now()),
                    ..BackgroundTaskUpdate::default()
                }
            }
            "error" => BackgroundTaskUpdate {
                state: Some(BackgroundTaskState::Failed),
                status: params["error"]["message"]
                    .as_str()
                    .or_else(|| params["message"].as_str())
                    .map(str::to_owned),
                completed_at: Some(SystemTime::now()),
                updated_at: Some(SystemTime::now()),
                ..BackgroundTaskUpdate::default()
            },
            _ => return false,
        };

        self.record(thread_id, update, true)
    }

    /// Hold the newest state of a thread whose relationship is still unknown.
    pub(super) fn hold_unrelated_notification(
        &mut self,
        thread_id: &str,
        method: &str,
        params: &Value,
    ) {
        let state = match method {
            "turn/started" => Some(BackgroundTaskState::Working),
            "turn/completed" => Some(match params["turn"]["status"].as_str() {
                Some("failed") => BackgroundTaskState::Failed,
                Some("interrupted") => BackgroundTaskState::Interrupted,
                _ => BackgroundTaskState::Done,
            }),
            "thread/status/changed" => thread_status_state(&params["status"]),
            _ => None,
        };
        let Some(state) = state else {
            return;
        };
        self.hold_pending(
            thread_id,
            BackgroundTaskUpdate {
                state: Some(state),
                updated_at: Some(SystemTime::now()),
                completed_at: state.is_terminal().then(SystemTime::now),
                ..BackgroundTaskUpdate::default()
            },
        );
    }

    pub(super) fn query_in_flight(&self) -> bool {
        !self.queries.is_empty()
    }

    pub(super) fn is_query(&self, rpc_id: u64) -> bool {
        self.queries.contains_key(&rpc_id)
    }

    /// Build one descendant page request. `None` when no parent thread is
    /// selected yet, so discovery simply waits for the thread id.
    pub(super) fn descendant_request(
        &mut self,
        rpc_id: u64,
        cursor: Option<&str>,
    ) -> Option<Value> {
        let root = self.root()?.to_owned();
        let starting_sequence = self.registry.as_ref()?.sequence();
        self.queries
            .insert(rpc_id, DescendantQuery { starting_sequence });
        if let Some(registry) = self.registry.as_mut() {
            registry.set_discovery(BackgroundTaskDiscoveryState::Loading);
        }

        // `ancestorThreadId` returns spawned descendants at any depth and
        // excludes the ancestor itself. An empty `modelProviders` list opts out
        // of the provider filter, and reading the state DB avoids rescanning
        // rollout files for metadata this view does not use.
        let mut params = json!({
            "ancestorThreadId": root,
            "sourceKinds": ["subAgentThreadSpawn"],
            "modelProviders": [],
            "useStateDbOnly": true,
            "limit": DESCENDANT_PAGE_LIMIT,
        });
        if let Some(cursor) = cursor {
            params["cursor"] = json!(cursor);
        }
        Some(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "thread/list",
            "params": params,
        }))
    }

    /// Fold one descendant page. Returns the cursor of the next page, if any.
    pub(super) fn apply_descendants(
        &mut self,
        rpc_id: u64,
        result: &Value,
    ) -> (bool, Option<String>) {
        let Some(query) = self.queries.remove(&rpc_id) else {
            return (false, None);
        };
        let Some(root) = self.root().map(str::to_owned) else {
            return (false, None);
        };

        let mut changed = false;
        for thread in result["data"].as_array().into_iter().flatten() {
            let Some(id) = thread["id"].as_str().filter(|id| *id != root) else {
                continue;
            };
            let parent = descendant_parent_id(thread);
            // Rows that do not chain back to the selected root belong to
            // another conversation on the same process.
            if !self.confirm(id, parent.as_deref().or(Some(root.as_str()))) {
                continue;
            }
            let id = id.to_owned();
            changed |= self.drain_pending(&id);

            // A listed thread reports only whether it is loaded and busy. A row
            // the live stream already described keeps its authoritative
            // lifecycle; one seen for the first time here is not running now,
            // and Stopped is the honest reading of an ended agent whose outcome
            // this listing does not carry.
            let known = self
                .registry
                .as_ref()
                .is_some_and(|registry| registry.contains(&BackgroundTaskKey::codex(&id)));
            let state = thread_status_state(&thread["status"])
                .or((!known).then_some(BackgroundTaskState::Stopped));
            let last_active = unix_seconds(thread, &["recencyAt", "updatedAt"]);

            let update = BackgroundTaskUpdate {
                refs: Some(BackgroundTaskRefs::Codex {
                    thread_id: id.clone(),
                    parent_thread_id: parent,
                }),
                state,
                display_name: text_field(thread, &["name", "agentNickname"]),
                agent_type: text_field(thread, &["agentRole"]),
                objective: text_field(thread, &["preview"]),
                depth: self.depth_of(&id),
                started_at: unix_seconds(thread, &["createdAt"]),
                // The listing has no completion timestamp, so a terminal row
                // borrows the thread's last activity as its end time.
                completed_at: state
                    .is_some_and(BackgroundTaskState::is_terminal)
                    .then_some(last_active)
                    .flatten(),
                updated_at: last_active,
                ..BackgroundTaskUpdate::default()
            };
            if let Some(registry) = self.registry.as_mut() {
                changed |= registry.merge_restored(
                    BackgroundTaskKey::codex(&id),
                    update,
                    query.starting_sequence,
                );
            }
        }

        let next_cursor = result["nextCursor"].as_str().map(str::to_owned);
        if next_cursor.is_none()
            && !self.query_in_flight()
            && let Some(registry) = self.registry.as_mut()
        {
            changed |= registry.set_discovery(BackgroundTaskDiscoveryState::Ready);
        }
        (changed, next_cursor)
    }

    /// Build a request for one descendant's stored conversation. `thread/read`
    /// does not resume or load the thread into this session, so the parent's
    /// thread id, turn state, approvals, and transcript are untouched by it.
    /// `None` when the child is not a confirmed descendant, or when a read for
    /// it is already in flight.
    pub(super) fn transcript_request(&mut self, rpc_id: u64, thread_id: &str) -> Option<Value> {
        if !self.confirmed.contains(thread_id) {
            return None;
        }
        if self.reads.values().any(|pending| pending == thread_id) {
            return None;
        }
        self.reads.insert(rpc_id, thread_id.to_owned());

        Some(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "thread/read",
            "params": {"threadId": thread_id, "includeTurns": true},
        }))
    }

    pub(super) fn is_transcript_read(&self, rpc_id: u64) -> bool {
        self.reads.contains_key(&rpc_id)
    }

    /// The descendant a completed read belongs to.
    pub(super) fn finish_transcript_read(&mut self, rpc_id: u64) -> Option<String> {
        self.reads.remove(&rpc_id)
    }

    pub(super) fn observe_raw_response_item(&mut self, thread_id: &str, item: &Value) -> bool {
        let Some(root) = self.root() else {
            return false;
        };
        if root == thread_id {
            return false;
        }
        self.launch_messages
            .observe(thread_id, item, self.confirmed.contains(thread_id))
    }

    pub(super) fn with_launch_message(&self, thread_id: &str, items: Vec<Item>) -> Vec<Item> {
        self.launch_messages.prepend(thread_id, items)
    }

    /// Record a failed descendant page. Known rows stay visible; the failure is
    /// only reported as unavailable when nothing can be shown at all.
    pub(super) fn fail_query(&mut self, rpc_id: u64, message: &str) -> bool {
        if self.queries.remove(&rpc_id).is_none() {
            return false;
        }
        let Some(registry) = self.registry.as_mut() else {
            return false;
        };
        if registry.is_empty() {
            return registry.set_discovery(BackgroundTaskDiscoveryState::Unavailable {
                message: message.to_owned(),
            });
        }
        registry.set_discovery(BackgroundTaskDiscoveryState::Ready)
    }
}

/// Thread id a notification is scoped to. Turn, item, and status notifications
/// carry it at the top level; notifications that deliver a whole thread carry
/// it inside that object instead.
pub(super) fn notification_thread_id(params: &Value) -> Option<&str> {
    params["threadId"]
        .as_str()
        .or_else(|| params["thread"]["id"].as_str())
}

fn descendant_parent_id(thread: &Value) -> Option<String> {
    thread["parentThreadId"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// One entry of a collaboration call's `agentsStates`: the child agent's own
/// lifecycle as the parent last observed it. This is the authoritative live
/// source, because a thread's runtime status only says whether it is loaded
/// and busy, never how its work ended.
fn collab_agent_state(state: &Value) -> Option<BackgroundTaskState> {
    Some(match state["status"].as_str()? {
        "pendingInit" => BackgroundTaskState::Starting,
        "running" => BackgroundTaskState::Working,
        "completed" => BackgroundTaskState::Done,
        "interrupted" => BackgroundTaskState::Interrupted,
        "shutdown" => BackgroundTaskState::Stopped,
        "errored" | "notFound" => BackgroundTaskState::Failed,
        _ => return None,
    })
}

/// Map a thread's runtime status onto the shared lifecycle. `idle` and
/// `notLoaded` mean the thread is not currently running, which is not by itself
/// an outcome, so they leave the known lifecycle alone.
fn thread_status_state(status: &Value) -> Option<BackgroundTaskState> {
    match status["type"].as_str()? {
        "active" if waiting_on_user(status) => Some(BackgroundTaskState::NeedsInput),
        "active" => Some(BackgroundTaskState::Working),
        "systemError" => Some(BackgroundTaskState::Failed),
        _ => None,
    }
}

fn waiting_on_user(status: &Value) -> bool {
    const FLAGS: [&str; 2] = ["waitingOnApproval", "waitingOnUserInput"];
    status["activeFlags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|flag| FLAGS.contains(&flag))
}

/// Short label for the child's most recent transcript item, used as the row
/// preview. Long agent text is truncated because the row is one line.
fn item_preview(item: &Value) -> Option<String> {
    const MAX_PREVIEW_CHARS: usize = 160;
    let text = item["text"]
        .as_str()
        .or_else(|| item["summary"].as_str())
        .or_else(|| item["command"].as_str())
        .or_else(|| item["title"].as_str())
        .or_else(|| item["type"].as_str())?;
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }
    Some(match text.char_indices().nth(MAX_PREVIEW_CHARS) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text,
    })
}

fn text_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value[*key]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    })
}

fn unix_seconds(value: &Value, keys: &[&str]) -> Option<SystemTime> {
    keys.iter()
        .find_map(|key| value[*key].as_u64())
        .filter(|seconds| *seconds > 0)
        .map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests;
