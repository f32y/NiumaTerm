mod router;

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock, Weak, mpsc};
use std::time::Duration;

use parking_lot::{Condvar, Mutex};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::LaunchConfig;
use crate::codex::app_server::host::router::Router;
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
        self.process.lock().try_write_line(&message)
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
