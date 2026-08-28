use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Weak, mpsc};
use std::time::Duration;

use parking_lot::{Condvar, Mutex};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::LaunchConfig;
use crate::launcher::AgentCli;
use crate::subprocess::JsonLineProcess;

const HOST_INIT_RPC_ID: u64 = 1;
const FIRST_HOST_RPC_ID: u64 = 2;
const START_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_EARLY_THREADS: usize = 64;
const MAX_EARLY_MESSAGES_PER_THREAD: usize = 32;
pub(super) const HOST_EXIT_METHOD: &str = "nmt/codexHostExited";

pub(super) type RegistrationId = u64;

type Delivery = Arc<dyn Fn(Value) + Send + Sync>;

static SHARED_HOST: LazyLock<SharedHostSlot> = LazyLock::new(SharedHostSlot::new);

struct SharedHostState {
    host: Weak<CodexHost>,
    starting: bool,
    attempt: u64,
    failed_attempts: VecDeque<(u64, String)>,
}

struct SharedHostSlot {
    state: Mutex<SharedHostState>,
    ready: Condvar,
}

impl SharedHostSlot {
    fn new() -> Self {
        Self {
            state: Mutex::new(SharedHostState {
                host: Weak::new(),
                starting: false,
                attempt: 0,
                failed_attempts: VecDeque::new(),
            }),
            ready: Condvar::new(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct HostKey {
    executable: String,
    arguments: Vec<String>,
    environment: Vec<(String, String)>,
}

struct HostBootstrap {
    key: HostKey,
    launch: LaunchConfig,
    credential_hashes: HashMap<String, [u8; 32]>,
    credential_values: Vec<String>,
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
}

struct RouterState {
    next_registration_id: RegistrationId,
    next_request_id: u64,
    sessions: HashMap<RegistrationId, Delivery>,
    pending_requests: HashMap<u64, PendingRoute>,
    server_requests: HashMap<u64, RegistrationId>,
    thread_owners: HashMap<String, RegistrationId>,
    root_by_owner: HashMap<RegistrationId, String>,
    early_messages: HashMap<String, VecDeque<Value>>,
    early_order: VecDeque<String>,
    startup_tx: Option<mpsc::SyncSender<Result<(), String>>>,
}

impl RouterState {
    fn new(startup_tx: mpsc::SyncSender<Result<(), String>>) -> Self {
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
                    self.server_requests.insert(id, owner);
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
        self.root_by_owner.insert(owner, thread_id.clone());
        self.claim_thread(owner, thread_id)
    }

    fn remove_thread(&mut self, thread_id: &str) {
        self.thread_owners.remove(thread_id);
        self.root_by_owner.retain(|_, root| root != thread_id);
        self.early_messages.remove(thread_id);
        self.early_order.retain(|candidate| candidate != thread_id);
    }
}

struct Router {
    state: Mutex<RouterState>,
    alive: AtomicBool,
    expected_shutdown: AtomicBool,
}

impl Router {
    fn new(startup_tx: mpsc::SyncSender<Result<(), String>>) -> Self {
        Self {
            state: Mutex::new(RouterState::new(startup_tx)),
            alive: AtomicBool::new(true),
            expected_shutdown: AtomicBool::new(false),
        }
    }

    fn register(&self, delivery: Delivery) -> RegistrationId {
        let mut state = self.state.lock();
        let id = state.next_registration_id;
        state.next_registration_id = state.next_registration_id.wrapping_add(1).max(1);
        state.sessions.insert(id, delivery);
        id
    }

    fn prepare_outgoing(&self, owner: RegistrationId, message: &mut Value) -> Result<(), String> {
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
            Some(expected_owner) if expected_owner == owner => Ok(()),
            Some(expected_owner) => {
                state.server_requests.insert(id, expected_owner);
                Err("Codex server request belongs to another Agent Tab".to_string())
            }
            None => Err("Codex server request is no longer pending".to_string()),
        }
    }

    fn handle_message(&self, message: Value) {
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
        state.server_requests.insert(id, owner);
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

    fn claim_descendants(
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

    fn detach(&self, owner: RegistrationId) -> bool {
        let mut state = self.state.lock();
        state.sessions.remove(&owner);
        state
            .pending_requests
            .retain(|_, route| route.owner != owner);
        state
            .server_requests
            .retain(|_, request_owner| *request_owner != owner);
        state
            .thread_owners
            .retain(|_, thread_owner| *thread_owner != owner);
        state.root_by_owner.remove(&owner);
        state.sessions.is_empty()
    }

    fn handle_stdout_closed(&self) {
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

pub(super) struct CodexHost {
    key: HostKey,
    credential_hashes: HashMap<String, [u8; 32]>,
    router: Arc<Router>,
    process: Mutex<JsonLineProcess>,
}

impl CodexHost {
    pub(super) fn acquire(
        launch: &LaunchConfig,
        catalog: &[LaunchConfig],
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Arc<Self>, String> {
        let bootstrap = HostBootstrap::from_launches(launch, catalog)?;
        let mut on_stderr = Some(on_stderr);
        loop {
            let mut shared = SHARED_HOST.state.lock();
            if let Some(host) = shared.host.upgrade()
                && host.router.alive.load(Ordering::Acquire)
            {
                host.ensure_compatible(launch, &bootstrap)?;
                return Ok(host);
            }

            if shared.starting {
                let attempt = shared.attempt;
                while shared.starting && shared.attempt == attempt {
                    SHARED_HOST.ready.wait(&mut shared);
                }
                if let Some((_, error)) = shared
                    .failed_attempts
                    .iter()
                    .find(|(failed_attempt, _)| *failed_attempt == attempt)
                {
                    return Err(error.clone());
                }
                continue;
            }

            shared.starting = true;
            shared.attempt = shared.attempt.wrapping_add(1).max(1);
            let attempt = shared.attempt;
            drop(shared);

            let started = Self::start(
                bootstrap,
                on_stderr
                    .take()
                    .expect("host startup callback is consumed by one attempt"),
            )
            .map(Arc::new);
            let mut shared = SHARED_HOST.state.lock();
            shared.starting = false;
            match &started {
                Ok(host) => shared.host = Arc::downgrade(host),
                Err(error) => {
                    shared.failed_attempts.push_back((attempt, error.clone()));
                    while shared.failed_attempts.len() > 8 {
                        shared.failed_attempts.pop_front();
                    }
                }
            }
            SHARED_HOST.ready.notify_all();
            return started;
        }
    }

    fn start(
        bootstrap: HostBootstrap,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        let credential_values = bootstrap.credential_values.clone();
        let stderr_credentials = credential_values.clone();
        let launcher = AgentCli::from_launch(&bootstrap.launch, "codex");
        let executable = launcher.executable().to_string();
        let command = launcher.command(["app-server"]);
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        let router = Arc::new(Router::new(startup_tx));
        let process = JsonLineProcess::spawn_with_stdout_closed(
            command,
            &format!("{executable} app-server"),
            "Codex",
            {
                let router = Arc::clone(&router);
                move |message| router.handle_message(message)
            },
            move |line| on_stderr(redact(&line, &stderr_credentials)),
            {
                let router = Arc::clone(&router);
                move || router.handle_stdout_closed()
            },
        )?;
        let host = Self {
            key: bootstrap.key,
            credential_hashes: bootstrap.credential_hashes,
            router,
            process: Mutex::new(process),
        };
        host.process.lock().write_line(&initialize_request());
        let initialized = startup_rx
            .recv_timeout(START_TIMEOUT)
            .map_err(|_| "Codex app-server did not initialize in time".to_string())?;
        initialized.map_err(|error| redact(&error, &credential_values))?;
        host.process.lock().write_line(&json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {},
        }));
        Ok(host)
    }

    fn ensure_compatible(
        &self,
        requested: &LaunchConfig,
        bootstrap: &HostBootstrap,
    ) -> Result<(), String> {
        if self.key != bootstrap.key {
            return Err(
                "This Codex profile uses app-server launch settings that differ from the live shared host; close existing Codex tabs before retrying"
                    .to_string(),
            );
        }
        if let Some(name) = requested
            .provider
            .as_ref()
            .and_then(|provider| provider.api_key_env.as_deref())
        {
            let normalized = normalize_env_name(name);
            let expected = bootstrap.credential_hashes.get(&normalized);
            if self.credential_hashes.get(&normalized) != expected {
                return Err(
                    "This Codex profile requires credentials that are not present in the live shared host; close existing Codex tabs before retrying"
                        .to_string(),
                );
            }
        }
        Ok(())
    }

    pub(super) fn register(
        &self,
        deliver: impl Fn(Value) + Send + Sync + 'static,
    ) -> RegistrationId {
        self.router.register(Arc::new(deliver))
    }

    pub(super) fn send(&self, owner: RegistrationId, mut message: Value) -> Result<(), String> {
        self.router.prepare_outgoing(owner, &mut message)?;
        self.process.lock().write_line(&message);
        Ok(())
    }

    pub(super) fn claim_descendants(
        &self,
        owner: RegistrationId,
        thread_ids: impl IntoIterator<Item = String>,
    ) {
        self.router.claim_descendants(owner, thread_ids);
    }

    pub(super) fn detach(&self, owner: RegistrationId) -> bool {
        self.router.detach(owner)
    }

    pub(super) fn shutdown(&self, timeout: Duration, force: bool) -> Result<(), String> {
        self.router.expected_shutdown.store(true, Ordering::Release);
        self.process.lock().shutdown(timeout, force)
    }
}

impl Drop for CodexHost {
    fn drop(&mut self) {
        self.router.expected_shutdown.store(true, Ordering::Release);
        let _ = self
            .process
            .get_mut()
            .shutdown(Duration::from_millis(250), true);
    }
}

impl HostBootstrap {
    fn from_launches(selected: &LaunchConfig, catalog: &[LaunchConfig]) -> Result<Self, String> {
        let mut launches = Vec::with_capacity(catalog.len() + 1);
        launches.push(selected);
        launches.extend(catalog.iter());

        let credential_names: HashSet<String> = launches
            .iter()
            .filter_map(|launch| {
                launch
                    .provider
                    .as_ref()
                    .and_then(|provider| provider.api_key_env.as_deref())
            })
            .map(normalize_env_name)
            .collect();
        let key = HostKey::from_launch(selected, &credential_names);
        let mut credentials = BTreeMap::<String, (String, String)>::new();
        let mut providers = BTreeMap::<String, (String, String)>::new();

        for launch in launches {
            if HostKey::from_launch(launch, &credential_names) != key {
                continue;
            }
            let Some(provider) = launch.provider.as_ref() else {
                continue;
            };
            let Some(name) = provider.api_key_env.as_deref() else {
                continue;
            };
            let Some(value) = effective_env(launch, name) else {
                continue;
            };
            let normalized = normalize_env_name(name);
            let provider_identity = (provider.id.clone(), provider.base_url.clone());
            if let Some(existing) = providers.get(&normalized)
                && existing != &provider_identity
            {
                return Err(format!(
                    "Codex provider credential name {name} is used by conflicting provider definitions"
                ));
            }
            providers.insert(normalized.clone(), provider_identity);
            if let Some((_, existing)) = credentials.get(&normalized)
                && existing != &value
            {
                return Err(format!(
                    "Codex provider credential name {name} resolves to more than one profile value"
                ));
            }
            credentials.insert(normalized, (name.to_string(), value));
        }

        let mut launch = selected.clone();
        launch.env = effective_process_env(selected, &credential_names)
            .into_values()
            .collect();
        launch.env.extend(credentials.values().cloned());
        let credential_values = credentials
            .values()
            .map(|(_, value)| value.clone())
            .collect();
        let credential_hashes = credentials
            .into_iter()
            .map(|(name, (_, value))| (name, hash_secret(&value)))
            .collect();
        Ok(Self {
            key,
            launch,
            credential_hashes,
            credential_values,
        })
    }
}

impl HostKey {
    fn from_launch(launch: &LaunchConfig, credential_names: &HashSet<String>) -> Self {
        let launcher = AgentCli::from_launch(launch, "codex");
        Self {
            executable: launcher
                .resolved_executable()
                .to_string_lossy()
                .to_lowercase(),
            arguments: launch.executable_args.clone(),
            environment: effective_process_env(launch, credential_names)
                .into_iter()
                .map(|(normalized, (_, value))| (normalized, value))
                .collect(),
        }
    }
}

fn effective_process_env(
    launch: &LaunchConfig,
    credential_names: &HashSet<String>,
) -> BTreeMap<String, (String, String)> {
    let mut environment = BTreeMap::new();
    for (name, value) in &launch.env {
        let normalized = normalize_env_name(name);
        if !credential_names.contains(&normalized) {
            environment.insert(normalized, (name.clone(), value.clone()));
        }
    }
    environment
}

fn effective_env(launch: &LaunchConfig, target: &str) -> Option<String> {
    launch
        .env
        .iter()
        .rev()
        .find(|(name, _)| name.eq_ignore_ascii_case(target))
        .map(|(_, value)| value.clone())
}

fn normalize_env_name(name: &str) -> String {
    name.trim().to_ascii_uppercase()
}

fn hash_secret(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

fn redact(text: &str, credential_values: &[String]) -> String {
    let mut redacted = text.to_string();
    let mut values: Vec<&str> = credential_values
        .iter()
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .collect();
    values.sort_unstable_by_key(|value| Reverse(value.len()));
    values.dedup();
    for value in values {
        redacted = redacted.replace(value, "<redacted>");
    }
    redacted
}

fn initialize_request() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": HOST_INIT_RPC_ID,
        "method": "initialize",
        "params": {
            "clientInfo": {"name": "NiumaTerm", "version": "0.1.0"},
            "capabilities": {"experimentalApi": true},
        },
    })
}

fn message_thread_id(message: &Value) -> Option<&str> {
    message["params"]["threadId"]
        .as_str()
        .or_else(|| message["params"]["thread"]["id"].as_str())
}

#[cfg(test)]
mod tests;
