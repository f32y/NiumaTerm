//! Which conversation a message from the shared app-server belongs to.
//!
//! Every Codex tab talks to one process, so a reply carries a request id and a
//! notification carries a thread id, and both have to reach the tab that asked
//! rather than all of them. A thread the server names before its tab has
//! claimed it is held until the claim arrives, because the two orders are both
//! legal and dropping the early traffic would lose the opening of a turn.

#[cfg(test)]
mod early_tests;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Instant;

use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::codex::app_server::host::{
    Delivery, FIRST_HOST_RPC_ID, HOST_EXIT_METHOD, HOST_INIT_RPC_ID, RegistrationId,
    message_thread_id,
};
use crate::deadline_timer::DeadlineTimer;
use crate::message_memory::OUTPUT_FAILURE_METHOD;
use crate::request_policy::RequestClass;
use crate::subprocess::InputTicket;

fn request_class(message: &Value) -> RequestClass {
    match message["method"].as_str() {
        Some("model/list" | "skills/list" | "thread/list" | "thread/read") => RequestClass::Query,
        Some("turn/interrupt" | "thread/unsubscribe") => RequestClass::Control,
        _ => RequestClass::Mutation,
    }
}

#[derive(Clone, Copy)]
enum RequestPurpose {
    ThreadStart(u64),
    ThreadResume(u64),
    ThreadFork(u64),
    SessionLocal(u64),
}

impl RequestPurpose {
    fn from_message(message: &Value, local_id: u64) -> Self {
        match message["method"].as_str() {
            Some("thread/start") => Self::ThreadStart(local_id),
            Some("thread/resume") => Self::ThreadResume(local_id),
            Some("thread/fork") => Self::ThreadFork(local_id),
            _ => Self::SessionLocal(local_id),
        }
    }

    fn local_id(self) -> u64 {
        match self {
            Self::ThreadStart(id)
            | Self::ThreadResume(id)
            | Self::ThreadFork(id)
            | Self::SessionLocal(id) => id,
        }
    }

    fn replaces_root(self) -> bool {
        matches!(
            self,
            Self::ThreadStart(_) | Self::ThreadResume(_) | Self::ThreadFork(_)
        )
    }
}

struct PendingRoute {
    owner: RegistrationId,
    purpose: RequestPurpose,
    class: RequestClass,
    deadline: Instant,
    input: Option<InputTicket>,
}

impl PendingRoute {
    fn timeout_response(&self) -> Value {
        let cancelled = self.input.as_ref().is_some_and(InputTicket::cancel);

        let message = if cancelled {
            "Codex request expired before writing and was cancelled; it was not sent.".to_string()
        } else {
            self.class.timeout_message("Codex")
        };

        json!({
            "id": self.purpose.local_id(),
            "error": {
                "message": message,
                "data": {"requestTimedOut": true, "notSent": cancelled},
            },
        })
    }
}

impl Drop for PendingRoute {
    fn drop(&mut self) {
        // Interrupt and unsubscribe requests must survive owner detachment:
        // they stop remote work and release subscriptions during tab closure.
        // Other retired requests must not start after their owner moved on.
        if self.class != RequestClass::Control
            && let Some(input) = &self.input
        {
            input.cancel();
        }
    }
}

struct ServerRequestRoute {
    owner: RegistrationId,
    thread_id: String,
}

struct RouterState {
    next_registration_id: RegistrationId,
    next_request_id: u64,
    sessions: HashMap<RegistrationId, Delivery>,
    pending_requests: HashMap<u64, PendingRoute>,
    server_requests: HashMap<u64, ServerRequestRoute>,
    thread_owners: HashMap<String, RegistrationId>,
    root_by_owner: HashMap<RegistrationId, String>,
    early_messages: EarlyMessages,
    startup_tx: Option<mpsc::SyncSender<Result<(), String>>>,
}

impl RouterState {
    pub(super) fn new(startup_tx: mpsc::SyncSender<Result<(), String>>) -> Self {
        Self {
            next_registration_id: 1,
            next_request_id: FIRST_HOST_RPC_ID,
            sessions: HashMap::new(),
            pending_requests: HashMap::new(),
            server_requests: HashMap::new(),
            thread_owners: HashMap::new(),
            root_by_owner: HashMap::new(),
            early_messages: EarlyMessages::default(),
            startup_tx: Some(startup_tx),
        }
    }

    fn allocate_request_id(&mut self) -> Option<u64> {
        let start = self.next_request_id;

        loop {
            let candidate = self.next_request_id;

            self.next_request_id = self.next_request_id.wrapping_add(1).max(FIRST_HOST_RPC_ID);

            if !self.pending_requests.contains_key(&candidate) && candidate != HOST_INIT_RPC_ID {
                return Some(candidate);
            }

            if self.next_request_id == start {
                return None;
            }
        }
    }

    fn delivery(&self, owner: RegistrationId) -> Option<Delivery> {
        self.sessions.get(&owner).cloned()
    }

    fn hold_early(&mut self, thread_id: &str, message: Value) {
        self.early_messages.hold(thread_id, message);
    }

    fn claim_thread(
        &mut self,
        owner: RegistrationId,
        thread_id: String,
    ) -> Result<Vec<(Delivery, Value)>, String> {
        if let Some(existing) = self.thread_owners.get(&thread_id) {
            return if *existing == owner {
                Ok(Vec::new())
            } else {
                Err(format!(
                    "Codex thread {thread_id} is already attached to another Agent Tab"
                ))
            };
        }

        self.thread_owners.insert(thread_id.clone(), owner);

        let Some(delivery) = self.delivery(owner) else {
            return Ok(Vec::new());
        };

        let early = self.early_messages.take(&thread_id);

        let closed = early.iter().any(|message| {
            matches!(
                message["method"].as_str(),
                Some("thread/closed" | "thread/deleted")
            )
        });

        let deliveries = early
            .into_iter()
            .map(|message| {
                if let (Some(id), Some(_)) = (message["id"].as_u64(), message["method"].as_str()) {
                    self.server_requests.insert(
                        id,
                        ServerRequestRoute {
                            owner,
                            thread_id: thread_id.clone(),
                        },
                    );
                }

                if message["method"].as_str() == Some("serverRequest/resolved")
                    && let Some(id) = message["params"]["requestId"].as_u64()
                {
                    self.server_requests.remove(&id);
                }

                (Arc::clone(&delivery), message)
            })
            .collect();

        if closed {
            self.remove_thread(&thread_id);
        }

        Ok(deliveries)
    }

    fn replace_root(
        &mut self,
        owner: RegistrationId,
        thread_id: String,
    ) -> Result<Vec<(Delivery, Value)>, String> {
        if let Some(existing) = self.thread_owners.get(&thread_id)
            && *existing != owner
        {
            return Err(format!(
                "Codex thread {thread_id} is already attached to another Agent Tab"
            ));
        }

        self.thread_owners
            .retain(|_, candidate| *candidate != owner);

        self.server_requests.retain(|_, route| route.owner != owner);
        self.root_by_owner.insert(owner, thread_id.clone());

        self.claim_thread(owner, thread_id)
    }

    fn remove_thread(&mut self, thread_id: &str) {
        self.server_requests
            .retain(|_, route| route.thread_id != thread_id);

        self.thread_owners.remove(thread_id);
        self.root_by_owner.retain(|_, root| root != thread_id);
        self.early_messages.forget(thread_id);
    }
}

pub(super) struct Router {
    state: Mutex<RouterState>,
    timer: Mutex<Option<DeadlineTimer>>,
    pub(super) alive: AtomicBool,
    pub(super) expected_shutdown: AtomicBool,
}

impl Router {
    pub(super) fn new(startup_tx: mpsc::SyncSender<Result<(), String>>) -> Self {
        Self {
            state: Mutex::new(RouterState::new(startup_tx)),
            timer: Mutex::new(None),
            alive: AtomicBool::new(true),
            expected_shutdown: AtomicBool::new(false),
        }
    }

    pub(super) fn register(&self, delivery: Delivery) -> RegistrationId {
        let mut state = self.state.lock();
        let id = state.next_registration_id;

        state.next_registration_id = state.next_registration_id.wrapping_add(1).max(1);
        state.sessions.insert(id, delivery);

        id
    }

    pub(super) fn start_timer(self: &Arc<Self>) -> Result<(), String> {
        let weak = Arc::downgrade(self);

        let timer = DeadlineTimer::new(move || {
            if let Some(router) = weak.upgrade() {
                router.expire_requests(Instant::now());
            }
        })
        .map_err(|error| format!("could not start Codex deadline timer: {error}"))?;

        let state = self.state.lock();

        *self.timer.lock() = Some(timer);
        self.refresh_timer(&state);

        Ok(())
    }

    fn refresh_timer(&self, state: &RouterState) {
        if let Some(timer) = self.timer.lock().as_ref() {
            timer.set(
                state
                    .pending_requests
                    .values()
                    .map(|route| route.deadline)
                    .min(),
            );
        }
    }

    pub(super) fn prepare_outgoing(
        &self,
        owner: RegistrationId,
        message: &mut Value,
    ) -> Result<(), String> {
        if !self.alive.load(Ordering::Acquire) {
            return Err("Codex app-server is not running".to_string());
        }

        let Some(id) = message["id"].as_u64() else {
            return Ok(());
        };

        if message["method"].is_string() {
            let mut state = self.state.lock();

            if !state.sessions.contains_key(&owner) {
                return Err("Codex session is detached".to_string());
            }

            let class = request_class(message);

            let global_id = state
                .allocate_request_id()
                .ok_or_else(|| "Codex app-server request IDs are exhausted".to_string())?;

            state.pending_requests.insert(
                global_id,
                PendingRoute {
                    owner,
                    purpose: RequestPurpose::from_message(message, id),
                    class,
                    deadline: Instant::now() + class.timeout(),
                    input: None,
                },
            );

            message["id"] = json!(global_id);
            self.refresh_timer(&state);

            return Ok(());
        }

        let mut state = self.state.lock();

        match state.server_requests.remove(&id) {
            Some(route) if route.owner == owner => Ok(()),

            Some(route) => {
                state.server_requests.insert(id, route);

                Err("Codex server request belongs to another Agent Tab".to_string())
            }

            None => Err("Codex server request is no longer pending".to_string()),
        }
    }

    pub(super) fn reject_outgoing(&self, id: u64) {
        let mut state = self.state.lock();

        state.pending_requests.remove(&id);
        self.refresh_timer(&state);
    }

    pub(super) fn attach_input(&self, id: u64, ticket: InputTicket) {
        if let Some(route) = self.state.lock().pending_requests.get_mut(&id) {
            route.input = Some(ticket);
        } else {
            ticket.cancel();
        }
    }

    pub(super) fn retain_requests(&self, owner: RegistrationId, ids: &[u64]) {
        let mut state = self.state.lock();

        state
            .pending_requests
            .retain(|_, route| route.owner != owner || ids.contains(&route.purpose.local_id()));

        self.refresh_timer(&state);
    }

    pub(super) fn expire_requests(&self, now: Instant) {
        let deliveries = {
            let mut state = self.state.lock();

            let expired: Vec<_> = state
                .pending_requests
                .extract_if(|_, route| route.deadline <= now)
                .map(|(_, route)| route)
                .collect();

            self.refresh_timer(&state);

            expired
                .into_iter()
                .filter_map(|route| {
                    let delivery = state.delivery(route.owner)?;

                    Some((delivery, route.timeout_response()))
                })
                .collect::<Vec<_>>()
        };

        // Delivery can re-enter routing, so no callback runs while state is locked.
        for (delivery, message) in deliveries {
            delivery(message);
        }
    }

    pub(super) fn on_message(&self, message: Value) {
        let deliveries = if let Some(method) = message["method"].as_str().map(str::to_string) {
            if message["id"].is_number() {
                self.route_server_request(message)
            } else {
                self.route_notification(&method, message)
            }
        } else if let Some(id) = message["id"].as_u64() {
            self.route_response(id, message)
        } else {
            Vec::new()
        };

        for (delivery, message) in deliveries {
            delivery(message);
        }
    }

    fn route_response(&self, id: u64, mut message: Value) -> Vec<(Delivery, Value)> {
        let mut state = self.state.lock();

        if id == HOST_INIT_RPC_ID {
            if let Some(tx) = state.startup_tx.take() {
                let result = message["error"]["message"]
                    .as_str()
                    .map(|error| Err(error.to_string()))
                    .unwrap_or(Ok(()));

                let _ = tx.send(result);
            }

            return Vec::new();
        }

        let Some(route) = state.pending_requests.remove(&id) else {
            return Vec::new();
        };

        self.refresh_timer(&state);
        message["id"] = json!(route.purpose.local_id());

        if route.deadline <= Instant::now() {
            message = route.timeout_response();
        }

        let Some(delivery) = state.delivery(route.owner) else {
            return Vec::new();
        };

        let mut deliveries = Vec::new();

        if route.purpose.replaces_root()
            && message["error"].is_null()
            && let Some(thread_id) = message["result"]["thread"]["id"].as_str()
        {
            match state.replace_root(route.owner, thread_id.to_string()) {
                Ok(early) => deliveries.extend(early),

                Err(error) => {
                    message = json!({
                        "id": route.purpose.local_id(),
                        "error": {"message": error},
                    });
                }
            }
        }

        deliveries.insert(0, (delivery, message));

        deliveries
    }

    fn route_server_request(&self, message: Value) -> Vec<(Delivery, Value)> {
        let Some(thread_id) = message_thread_id(&message).map(str::to_string) else {
            return Vec::new();
        };

        let Some(id) = message["id"].as_u64() else {
            return Vec::new();
        };

        let mut state = self.state.lock();

        let Some(owner) = state.thread_owners.get(&thread_id).copied() else {
            state.hold_early(&thread_id, message);

            return Vec::new();
        };

        let Some(delivery) = state.delivery(owner) else {
            return Vec::new();
        };

        state
            .server_requests
            .insert(id, ServerRequestRoute { owner, thread_id });

        vec![(delivery, message)]
    }

    fn route_notification(&self, method: &str, message: Value) -> Vec<(Delivery, Value)> {
        if method == OUTPUT_FAILURE_METHOD
            && let Some(startup) = self.state.lock().startup_tx.take()
        {
            let reason = message["params"]["message"]
                .as_str()
                .unwrap_or("Codex protocol reader failed");

            let _ = startup.send(Err(reason.to_string()));
        }

        let Some(thread_id) = message_thread_id(&message).map(str::to_string) else {
            return self
                .state
                .lock()
                .sessions
                .values()
                .cloned()
                .map(|delivery| (delivery, message.clone()))
                .collect();
        };

        let mut state = self.state.lock();
        let mut deliveries = Vec::new();

        if method == "serverRequest/resolved"
            && let Some(id) = message["params"]["requestId"].as_u64()
            && state
                .server_requests
                .get(&id)
                .is_some_and(|route| route.thread_id == thread_id)
        {
            state.server_requests.remove(&id);
        }

        if method == "thread/started"
            && !state.thread_owners.contains_key(&thread_id)
            && let Some(parent_id) = message["params"]["thread"]["parentThreadId"].as_str()
            && let Some(owner) = state.thread_owners.get(parent_id).copied()
            && let Ok(early) = state.claim_thread(owner, thread_id.clone())
        {
            deliveries.extend(early);
        }

        let Some(owner) = state.thread_owners.get(&thread_id).copied() else {
            state.hold_early(&thread_id, message);

            return Vec::new();
        };

        let Some(delivery) = state.delivery(owner) else {
            return Vec::new();
        };

        deliveries.push((delivery, message));

        if matches!(method, "thread/closed" | "thread/deleted") {
            state.remove_thread(&thread_id);
        }

        deliveries
    }

    pub(super) fn claim_descendants(
        &self,
        owner: RegistrationId,
        thread_ids: impl IntoIterator<Item = String>,
    ) {
        let deliveries = {
            let mut state = self.state.lock();
            let mut deliveries = Vec::new();

            for thread_id in thread_ids {
                if let Ok(early) = state.claim_thread(owner, thread_id) {
                    deliveries.extend(early);
                }
            }

            deliveries
        };

        for (delivery, message) in deliveries {
            delivery(message);
        }
    }

    pub(super) fn detach(&self, owner: RegistrationId) -> bool {
        let mut state = self.state.lock();

        state.sessions.remove(&owner);

        state
            .pending_requests
            .retain(|_, route| route.owner != owner);

        state
            .server_requests
            .retain(|_, route| route.owner != owner);

        state
            .thread_owners
            .retain(|_, thread_owner| *thread_owner != owner);

        state.root_by_owner.remove(&owner);
        self.refresh_timer(&state);

        state.sessions.is_empty()
    }

    pub(super) fn on_stdout_closed(&self) {
        if !self.alive.swap(false, Ordering::AcqRel) {
            return;
        }

        let (startup_tx, deliveries) = {
            let mut state = self.state.lock();
            let startup_tx = state.startup_tx.take();

            let deliveries = if self.expected_shutdown.load(Ordering::Acquire) {
                Vec::new()
            } else {
                state.sessions.values().cloned().collect::<Vec<_>>()
            };

            state.pending_requests.clear();
            self.refresh_timer(&state);
            state.server_requests.clear();
            state.thread_owners.clear();
            state.root_by_owner.clear();
            state.early_messages.clear();

            (startup_tx, deliveries)
        };

        if let Some(tx) = startup_tx {
            let _ = tx.send(Err(
                "Codex app-server exited during initialization".to_string()
            ));
        }

        let message = json!({
            "method": HOST_EXIT_METHOD,
            "params": {"message": "Codex app-server stopped unexpectedly"},
        });

        for delivery in deliveries {
            delivery(message.clone());
        }
    }
}

// Retain early traffic until its owning tab can receive it.

#[derive(Default)]
struct EarlyMessages {
    threads: HashMap<String, Vec<Value>>,
}

impl EarlyMessages {
    fn hold(&mut self, thread_id: &str, value: Value) {
        // Ownership can arrive after arbitrary amounts of activity. Eviction
        // would lose turn state or an approval the server is waiting on.
        self.threads
            .entry(thread_id.to_owned())
            .or_default()
            .push(value);
    }

    fn take(&mut self, thread_id: &str) -> Vec<Value> {
        self.threads.remove(thread_id).unwrap_or_default()
    }

    fn forget(&mut self, thread_id: &str) {
        self.threads.remove(thread_id);
    }

    fn clear(&mut self) {
        self.threads.clear();
    }
}
