mod router;

#[cfg(test)]
mod tests;

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::time::Duration;

use futures::future::{BoxFuture, Shared};
use futures::{FutureExt as _, TryFutureExt as _};
use parking_lot::Mutex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;
use tokio::time::timeout;

use crate::LaunchConfig;
use crate::codex::app_server::host::router::Router;
use crate::launcher::AgentCli;
use crate::subprocess::{DROP_SHUTDOWN_GRACE, JsonLineProcess};

const HOST_INIT_RPC_ID: u64 = 1;
const FIRST_HOST_RPC_ID: u64 = 2;
const START_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const HOST_EXIT_METHOD: &str = "nmt/codexHostExited";

pub(super) type RegistrationId = u64;

type Delivery = Arc<dyn Fn(Value) + Send + Sync>;

static SHARED_HOSTS: Mutex<Vec<(HostKey, HostSlot)>> = Mutex::new(Vec::new());

type HostSlot = Arc<Mutex<SharedHost>>;

/// Tabs with the same launch settings share a weak host reference so it stops
/// with its last tab. Concurrent acquisitions of the same launch await one
/// startup result, including failures. If the initiator is cancelled, the next
/// acquisition of that launch resumes the pending startup.
enum SharedHost {
    Idle(Weak<CodexHost>),
    Starting(Shared<BoxFuture<'static, Result<Arc<CodexHost>, String>>>),
}

#[derive(PartialEq, Eq)]
struct HostKey {
    executable: String,
    arguments: Vec<String>,
    endpoint: Option<String>,
    // Hash values so registry entries do not retain copies of API keys.
    environment: Vec<(String, [u8; 32])>,
}

pub(super) struct CodexHost {
    router: Arc<Router>,
    process: Mutex<JsonLineProcess>,
}

fn host_slot(launch: &LaunchConfig) -> HostSlot {
    let key = HostKey::from_launch(launch);

    let mut hosts = SHARED_HOSTS.lock();

    hosts.retain(|(_, slot)| {
        Arc::strong_count(slot) > 1
            || match &*slot.lock() {
                SharedHost::Idle(host) => host.strong_count() > 0,
                SharedHost::Starting(_) => true,
            }
    });

    if let Some((_, slot)) = hosts.iter().find(|(candidate, _)| *candidate == key) {
        return Arc::clone(slot);
    }

    let slot = Arc::new(Mutex::new(SharedHost::Idle(Weak::new())));

    hosts.push((key, Arc::clone(&slot)));

    slot
}

impl CodexHost {
    pub(super) fn retain_requests(&self, owner: RegistrationId, ids: &[u64]) {
        self.router.retain_requests(owner, ids);
    }

    pub(super) async fn acquire(
        launch: &LaunchConfig,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Arc<Self>, String> {
        let slot = host_slot(launch);

        let start = {
            let mut shared = slot.lock();

            if let SharedHost::Idle(host) = &*shared
                && let Some(host) = host.upgrade()
                && host.router.alive.load(Ordering::Acquire)
            {
                return Ok(host);
            }

            match &*shared {
                SharedHost::Starting(start) => start.clone(),
                SharedHost::Idle(_) => {
                    let start = Self::start(launch.clone(), on_stderr)
                        .map_ok(Arc::new)
                        .boxed()
                        .shared();

                    *shared = SharedHost::Starting(start.clone());

                    start
                }
            }
        };

        let started = start.clone().await;

        {
            let mut shared = slot.lock();

            // A later start may already have replaced this one.
            if let SharedHost::Starting(current) = &*shared
                && current.ptr_eq(&start)
            {
                *shared = SharedHost::Idle(
                    started
                        .as_ref()
                        .map_or_else(|_| Weak::new(), Arc::downgrade),
                );
            }
        }

        started
    }

    async fn start(
        launch: LaunchConfig,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        let credential_values: Vec<String> = launch
            .provider
            .as_ref()
            .and_then(|provider| provider.api_key_env.as_deref())
            .and_then(|name| effective_env(&launch, name))
            .into_iter()
            .collect();

        let stderr_credentials = credential_values.clone();
        let launcher = AgentCli::from_launch(&launch, "codex");
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
        nmt_platform::runtime().spawn(self.shutdown(DROP_SHUTDOWN_GRACE, true));
    }
}

impl HostKey {
    fn from_launch(launch: &LaunchConfig) -> Self {
        let launcher = AgentCli::from_launch(launch, "codex");

        let environment: BTreeMap<_, _> = launch
            .env
            .iter()
            .map(|(name, value)| (normalize_env_name(name), hash_secret(value)))
            .collect();

        Self {
            executable: launcher
                .resolved_executable()
                .to_string_lossy()
                .to_lowercase(),
            arguments: launch.executable_args.clone(),
            endpoint: launch
                .provider
                .as_ref()
                .map(|provider| provider.base_url.clone()),
            environment: environment.into_iter().collect(),
        }
    }
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
