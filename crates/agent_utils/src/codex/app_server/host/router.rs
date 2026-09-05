//! Which conversation a message from the shared app-server belongs to.
//!
//! Every Codex tab talks to one process, so a reply carries a request id and a
//! notification carries a thread id, and both have to reach the tab that asked
//! rather than all of them. A thread the server names before its tab has
//! claimed it is held until the claim arrives, because the two orders are both
//! legal and dropping the early traffic would lose the opening of a turn.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::codex::app_server::host::{
    Delivery, FIRST_HOST_RPC_ID, HOST_EXIT_METHOD, HOST_INIT_RPC_ID, MAX_EARLY_MESSAGES_PER_THREAD,
    MAX_EARLY_THREADS, RegistrationId, message_thread_id,
};

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
    early_messages: HashMap<String, VecDeque<Value>>,
    early_order: VecDeque<String>,
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
            early_messages: HashMap::new(),
            early_order: VecDeque::new(),
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
        if !self.early_messages.contains_key(thread_id) {
            while self.early_order.len() >= MAX_EARLY_THREADS {
                if let Some(oldest) = self.early_order.pop_front() {
                    self.early_messages.remove(&oldest);
                }
            }
            self.early_order.push_back(thread_id.to_string());
        }
        let messages = self
            .early_messages
            .entry(thread_id.to_string())
            .or_default();
        if messages.len() == MAX_EARLY_MESSAGES_PER_THREAD {
            messages.pop_front();
        }
        messages.push_back(message);
    }

    fn claim_thread(
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
        self.thread_owners.insert(thread_id.clone(), owner);
        let Some(delivery) = self.delivery(owner) else {
            return Ok(Vec::new());
        };
        self.early_order.retain(|candidate| candidate != &thread_id);
        let early = self.early_messages.remove(&thread_id).unwrap_or_default();
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
        self.early_messages.remove(thread_id);
        self.early_order.retain(|candidate| candidate != thread_id);
    }
}

pub(super) struct Router {
    state: Mutex<RouterState>,
    pub(super) alive: AtomicBool,
    pub(super) expected_shutdown: AtomicBool,
}

impl Router {
    pub(super) fn new(startup_tx: mpsc::SyncSender<Result<(), String>>) -> Self {
        Self {
            state: Mutex::new(RouterState::new(startup_tx)),
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
            let global_id = state
                .allocate_request_id()
                .ok_or_else(|| "Codex app-server request IDs are exhausted".to_string())?;
            state.pending_requests.insert(
                global_id,
                PendingRoute {
                    owner,
                    purpose: RequestPurpose::from_message(message, id),
                },
            );
            message["id"] = json!(global_id);
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

    pub(super) fn handle_message(&self, message: Value) {
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
        message["id"] = json!(route.purpose.local_id());
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
        state.sessions.is_empty()
    }

    pub(super) fn handle_stdout_closed(&self) {
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
            state.server_requests.clear();
            state.thread_owners.clear();
            state.root_by_owner.clear();
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
