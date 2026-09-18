mod router;

#[cfg(test)]
mod tests;

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, oneshot};
use tokio::time::timeout;

use crate::LaunchConfig;
use crate::codex::app_server::host::router::Router;
use crate::launcher::AgentCli;
use crate::subprocess::JsonLineProcess;

const HOST_INIT_RPC_ID: u64 = 1;
const FIRST_HOST_RPC_ID: u64 = 2;
const START_TIMEOUT: Duration = Duration::from_secs(30);
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

    /// Wakes every waiter created before a startup attempt settles.
    ready: Notify,
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
            ready: Notify::new(),
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

pub(super) struct CodexHost {
    key: HostKey,
    credential_hashes: HashMap<String, [u8; 32]>,
    router: Arc<Router>,
    process: Mutex<JsonLineProcess>,
}

impl CodexHost {
    pub(super) fn retain_requests(&self, owner: RegistrationId, ids: &[u64]) {
        self.router.retain_requests(owner, ids);
    }

    pub(super) async fn acquire(
        launch: &LaunchConfig,
        catalog: &[LaunchConfig],
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Arc<Self>, String> {
        let bootstrap = HostBootstrap::from_launches(launch, catalog)?;

        loop {
            // Created before the state is read, so a startup that settles in
            // between still wakes this waiter.
            let settled = SHARED_HOST.ready.notified();

            let waiting_for = {
                let mut shared = SHARED_HOST.state.lock();

                if let Some(host) = shared.host.upgrade()
                    && host.router.alive.load(Ordering::Acquire)
                {
                    host.ensure_compatible(launch, &bootstrap)?;

                    return Ok(host);
                }

                if shared.starting {
                    Some(shared.attempt)
                } else {
                    shared.starting = true;
                    shared.attempt = shared.attempt.wrapping_add(1).max(1);

                    None
                }
            };

            if let Some(attempt) = waiting_for {
                settled.await;

                if let Some((_, error)) = SHARED_HOST
                    .state
                    .lock()
                    .failed_attempts
                    .iter()
                    .find(|(failed_attempt, _)| *failed_attempt == attempt)
                {
                    return Err(error.clone());
                }

                continue;
            }

            let mut attempt = StartAttempt::claim();

            let started = Self::start(bootstrap, on_stderr).await.map(Arc::new);

            attempt.settle(&started);

            return started;
        }
    }

    async fn start(
        bootstrap: HostBootstrap,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        let credential_values = bootstrap.credential_values.clone();
        let stderr_credentials = credential_values.clone();
        let launcher = AgentCli::from_launch(&bootstrap.launch, "codex");
        let executable = launcher.executable().to_string();
        // Goal scheduling must be available to the tab's native goal controls.
        // This enables it only in the child process, without changing user files.
        let command = launcher.command(["-c", "features.goals=true", "app-server"]);
        let (startup_tx, startup_rx) = oneshot::channel();
        let router = Arc::new(Router::new(startup_tx));

        let process = JsonLineProcess::spawn_with_stdout_closed(
            command,
            &format!("{executable} app-server"),
            "Codex",
            {
                let router = Arc::clone(&router);

                move |message| router.on_message(message)
            },
            move |line| on_stderr(redact(&line, &stderr_credentials)),
            {
                let router = Arc::clone(&router);

                move || router.on_stdout_closed()
            },
        )?;

        let host = Self {
            key: bootstrap.key,
            credential_hashes: bootstrap.credential_hashes,
            router,
            process: Mutex::new(process),
        };

        host.router.start_timer();

        host.process
            .lock()
            .write_line(initialize_request())
            .map_err(|error| error.to_string())?;

        let initialized = timeout(START_TIMEOUT, startup_rx)
            .await
            .ok()
            .and_then(Result::ok)
            .ok_or_else(|| "Codex app-server did not initialize in time".to_string())?;

        initialized.map_err(|error| redact(&error, &credential_values))?;

        host.process
            .lock()
            .write_line(json!({
                "jsonrpc": "2.0",
                "method": "initialized",
                "params": {},
            }))
            .map_err(|error| error.to_string())?;

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

        let request_id = message["method"]
            .is_string()
            .then(|| message["id"].as_u64())
            .flatten();

        let result = match message["method"].as_str() {
            None => self.process.lock().write_line(message).map(|_| None),
            Some("turn/interrupt" | "thread/unsubscribe") => {
                let mut process = self.process.lock();

                let result = process.write_tracked(vec![message]);

                if result.is_err() {
                    process.abort();
                }

                result.map(Some)
            }
            _ => self.process.lock().write_tracked(vec![message]).map(Some),
        };

        if result.is_err()
            && let Some(id) = request_id
        {
            self.router.reject_outgoing(id);
        }

        match result {
            Ok(ticket) => {
                if let Some(id) = request_id
                    && let Some(ticket) = ticket
                {
                    self.router.attach_input(id, ticket);
                }

                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
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

    pub(super) fn shutdown(
        &self,
        timeout: Duration,
        force: bool,
    ) -> impl Future<Output = Result<(), String>> + Send + use<> {
        self.router.expected_shutdown.store(true, Ordering::Release);

        self.process.lock().shutdown(timeout, force)
    }
}

impl Drop for CodexHost {
    fn drop(&mut self) {
        self.router.expected_shutdown.store(true, Ordering::Release);

        nmt_runtime::handle().spawn(
            self.process
                .get_mut()
                .shutdown(Duration::from_millis(250), true),
        );
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

/// The startup attempt this caller claimed. Settling records the outcome and
/// wakes waiters; a startup dropped before it settles is recorded as failed,
/// so waiters are released instead of waiting for an attempt that never ends.
struct StartAttempt {
    attempt: u64,
    settled: bool,
}

impl StartAttempt {
    fn claim() -> Self {
        Self {
            attempt: SHARED_HOST.state.lock().attempt,
            settled: false,
        }
    }

    fn settle(&mut self, started: &Result<Arc<CodexHost>, String>) {
        self.settled = true;

        let mut shared = SHARED_HOST.state.lock();

        shared.starting = false;

        match started {
            Ok(host) => shared.host = Arc::downgrade(host),
            Err(error) => record_failure(&mut shared, self.attempt, error.clone()),
        }

        drop(shared);

        SHARED_HOST.ready.notify_waiters();
    }
}

impl Drop for StartAttempt {
    fn drop(&mut self) {
        if self.settled {
            return;
        }

        let mut shared = SHARED_HOST.state.lock();

        shared.starting = false;

        record_failure(
            &mut shared,
            self.attempt,
            "Codex app-server startup was cancelled".to_string(),
        );

        drop(shared);

        SHARED_HOST.ready.notify_waiters();
    }
}

fn record_failure(shared: &mut SharedHostState, attempt: u64, error: String) {
    shared.failed_attempts.push_back((attempt, error));

    while shared.failed_attempts.len() > 8 {
        shared.failed_attempts.pop_front();
    }
}
