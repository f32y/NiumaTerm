//! The `dsh web` host process: one per application, shared by every DeepSeek
//! tab, because its event stream is all-session aggregated and a second host
//! would pay the Node start cost again for nothing.

use std::io::{BufRead as _, BufReader};
use std::process::{Child, Stdio};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, Weak};
use std::thread;
use std::time::Duration;

use parking_lot::Mutex;

use crate::deepseek::api::ApiClient;
use crate::launcher::AgentCli;
use crate::subprocess::KillOnCloseJob;

/// The host announces where it bound on stdout, which is the only way to learn
/// the port when it is asked to pick one. Everything before this prefix on the
/// line is ignored so a future banner cannot break discovery.
const ADDRESS_MARKER: &str = "http://";

/// Node startup plus profile initialization measured at about 1.1 s on a warm
/// machine. The wait is generous because expiring is reported as a failed
/// start, and a slow first run is not a failure.
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// How many stderr lines are kept to explain a failed start. The host writes
/// its own diagnostics there, and a bounded tail keeps a runaway log from
/// growing without limit while a tab is open.
const RETAINED_STDERR_LINES: usize = 40;

/// Why a tab has no usable harness. These are distinct because the answers
/// differ: a missing install is something the user resolves once, while a
/// failed start usually names a real error in the host's own output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostError {
    /// `dsh` could not be resolved. It is a user-installed dependency, so this
    /// is not something the application offers to fix.
    NotInstalled(String),
    /// The executable resolved but no serving host came out of it.
    FailedToStart(String),
}

impl HostError {
    pub fn message(&self) -> &str {
        match self {
            Self::NotInstalled(message) | Self::FailedToStart(message) => message,
        }
    }
}

/// A running host. Dropping it terminates the process tree through the Job
/// Object: the harness is launched through an npm shim, so killing only the
/// shim would strand the Node process that actually holds the port.
///
/// There is deliberately no graceful shutdown handshake. The harness's stdio
/// carries no control protocol once it is serving, and its stdin EOF was
/// observed not to end a process that had already run a turn.
pub struct Host {
    client: ApiClient,
    base: String,
    stderr: Arc<Mutex<Vec<String>>>,
    /// Held for its Drop: releasing the job terminates the host and every
    /// descendant it spawned.
    _job: KillOnCloseJob,
    child: Mutex<Child>,
}

/// The one host every DeepSeek tab shares, held weakly so it stops when the
/// last tab lets go. A strong static would keep Node running for the rest of
/// the application's life after the last DeepSeek tab closed.
static SHARED: Mutex<Weak<Host>> = Mutex::new(Weak::new());

/// Hand out the running host, starting it if no tab currently holds one.
///
/// One host serves every tab because its event stream is aggregated across
/// sessions: a second process would pay the Node start cost again and deliver
/// the same frames twice.
pub fn shared(launch: &crate::LaunchConfig) -> Result<Arc<Host>, HostError> {
    let mut slot = SHARED.lock();

    if let Some(running) = slot.upgrade()
        && running.is_running()
    {
        return Ok(running);
    }

    let host = Arc::new(Host::start(launch)?);
    *slot = Arc::downgrade(&host);

    Ok(host)
}

impl Host {
    /// Start `dsh web` on a loopback port the operating system picks, and wait
    /// until it reports where it bound.
    ///
    /// Binding an ephemeral port avoids racing other local software for a fixed
    /// one; the cost is that the address can only be learned from the host, so
    /// the wait below is also the start-failure detector.
    pub fn start(launch: &crate::LaunchConfig) -> Result<Self, HostError> {
        let cli = AgentCli::from_launch(launch, DEFAULT_EXECUTABLE);
        // A bare name that PATH cannot resolve comes back as the configured
        // spelling, which is not a file; that is the missing-installation case
        // rather than a start failure, and it has a different answer.
        if !cli.resolved_executable().is_file() {
            return Err(HostError::NotInstalled(cli.executable().to_string()));
        }

        let mut command = cli.command(["web", "--port", "0"]);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command
            .spawn()
            .map_err(|error| HostError::FailedToStart(error.to_string()))?;
        let job = KillOnCloseJob::attach_or_kill(&mut child).map_err(HostError::FailedToStart)?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HostError::FailedToStart("the host produced no output".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| HostError::FailedToStart("the host produced no output".to_string()))?;

        let retained = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&retained);
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut lines = sink.lock();
                if lines.len() == RETAINED_STDERR_LINES {
                    lines.remove(0);
                }
                lines.push(line);
            }
        });

        // The address arrives on one line and the rest of stdout is of no
        // further use, so the reader thread reports that line and then drains
        // the pipe to keep the host from blocking on a full buffer.
        let (address_tx, address_rx) = channel();
        thread::spawn(move || {
            let mut announced = false;
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if announced {
                    continue;
                }
                if let Some(address) = address_in(&line) {
                    announced = address_tx.send(address).is_ok();
                }
            }
        });

        let base = match address_rx.recv_timeout(START_TIMEOUT) {
            Ok(address) => address,
            Err(reason) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(HostError::FailedToStart(start_failure(reason, &retained)));
            }
        };

        let client = ApiClient::new(base.clone()).map_err(HostError::FailedToStart)?;

        Ok(Self {
            client,
            base,
            stderr: retained,
            _job: job,
            child: Mutex::new(child),
        })
    }

    pub(crate) fn client(&self) -> &ApiClient {
        &self.client
    }

    /// The origin the host bound to, for the WebSocket downlinks.
    pub(crate) fn base(&self) -> &str {
        &self.base
    }

    /// Whether the host is still serving. A host that exited takes every open
    /// tab's session with it, so tabs ask this rather than discovering it on
    /// their next call.
    pub fn is_running(&self) -> bool {
        matches!(self.child.lock().try_wait(), Ok(None))
    }

    /// The host's own diagnostics, for reporting an exit the user did not ask
    /// for.
    pub fn stderr_tail(&self) -> String {
        self.stderr.lock().join("\n")
    }
}

/// Bare `dsh` resolves through PATH, which on Windows also finds the `dsh.cmd`
/// shim npm installs.
pub const DEFAULT_EXECUTABLE: &str = "dsh";

/// Launcher for a profile that runs the harness from its published package
/// rather than an installed binary. `-y` is not optional: the host is spawned
/// with a null stdin, so npx's prompt before fetching a package it does not
/// have cached could never be answered.
pub const NPX_EXECUTABLE: &str = "npx";
pub const NPX_ARGUMENTS: [&str; 2] = ["-y", "@deepseek-ai/dsh"];

fn address_in(line: &str) -> Option<String> {
    let start = line.find(ADDRESS_MARKER)?;
    let address = line[start..].trim();

    (!address.is_empty()).then(|| address.to_string())
}

fn start_failure(reason: RecvTimeoutError, stderr: &Mutex<Vec<String>>) -> String {
    let detail = stderr.lock().join("\n");
    let cause = match reason {
        // The sender is dropped when the reader thread ends, which happens when
        // stdout closes: the host exited before it bound a port.
        RecvTimeoutError::Disconnected => "the harness host exited before it started serving",
        RecvTimeoutError::Timeout => "the harness host did not report an address in time",
    };

    if detail.trim().is_empty() {
        cause.to_string()
    } else {
        format!("{cause}: {detail}")
    }
}
