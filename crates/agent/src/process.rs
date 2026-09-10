use std::process;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use getrandom::fill;

use crate::AgentRoute;

pub const AGENT_HOOK_PROTOCOL_VERSION: u32 = 1;

pub const AGENT_ROUTE_ENV: &str = "NMT_AGENT_ROUTE";
pub const AGENT_HOOK_TOKEN_ENV: &str = "NMT_AGENT_HOOK_TOKEN";
pub const AGENT_HOOK_VERSION_ENV: &str = "NMT_AGENT_HOOK_VERSION";
pub const AGENT_HOOK_EXE_ENV: &str = "NMT_AGENT_HOOK_EXE";
pub const AGENT_TESTING_ENV: &str = "NMT_TESTING";

pub struct AgentProcess {
    nonce: String,
    pub(super) hook_token: String,
    hook_executable: OnceLock<String>,
    testing: AtomicBool,
    next_route: AtomicU64,
    next_notification: AtomicU64,
}

impl AgentProcess {
    pub(super) fn new() -> Self {
        let mut nonce = [0u8; 16];
        let mut hook_token = [0u8; 32];

        fill(&mut nonce).expect("Windows cryptographic random source");
        fill(&mut hook_token).expect("Windows cryptographic random source");

        Self {
            nonce: format!("{:x}-{}", process::id(), hex(&nonce)),
            hook_token: hex(&hook_token),
            hook_executable: OnceLock::new(),
            testing: AtomicBool::new(false),
            next_route: AtomicU64::new(1),
            next_notification: AtomicU64::new(1),
        }
    }

    pub fn allocate_route(&self) -> AgentRoute {
        let counter = self.next_route.fetch_add(1, Ordering::Relaxed);
        AgentRoute(format!("{}-{counter:x}", self.nonce))
    }

    pub fn next_notification_counter(&self) -> u64 {
        self.next_notification.fetch_add(1, Ordering::Relaxed)
    }

    pub fn hook_token(&self) -> &str {
        &self.hook_token
    }

    pub fn process_instance(&self) -> &str {
        &self.nonce
    }

    pub fn set_testing(&self, testing: bool) {
        self.testing.store(testing, Ordering::Relaxed);
    }

    /// Absolute path of the hook CLI binary, exported to every pane so
    /// externally configured agent hooks can locate it via `$NMT_AGENT_HOOK_EXE`
    /// without baking an install path into their configuration.
    pub fn set_hook_executable(&self, path: String) {
        let _ = self.hook_executable.set(path);
    }

    /// Installers use the same absolute binary path exported to pane children,
    /// so their registrations keep working when NiumaTerm is installed in a
    /// directory that is not on `PATH`.
    pub fn hook_executable(&self) -> Option<&str> {
        self.hook_executable.get().map(String::as_str)
    }

    pub fn environment_for(&self, route: &AgentRoute) -> Vec<(String, String)> {
        let mut environment = vec![
            (AGENT_ROUTE_ENV.into(), route.as_str().into()),
            (AGENT_HOOK_TOKEN_ENV.into(), self.hook_token.clone()),
            (
                AGENT_HOOK_VERSION_ENV.into(),
                AGENT_HOOK_PROTOCOL_VERSION.to_string(),
            ),
        ];

        if let Some(path) = self.hook_executable.get() {
            environment.push((AGENT_HOOK_EXE_ENV.into(), path.clone()));
        }

        if self.testing.load(Ordering::Relaxed) {
            environment.push((AGENT_TESTING_ENV.into(), "1".into()));
        }

        environment
    }
}

pub fn agent_process() -> &'static AgentProcess {
    static PROCESS: OnceLock<AgentProcess> = OnceLock::new();

    PROCESS.get_or_init(AgentProcess::new)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut output = String::with_capacity(bytes.len() * 2);

    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }

    output
}
