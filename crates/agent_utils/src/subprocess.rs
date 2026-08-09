//! Shared child-process management for agent CLI subprocesses: kill-on-close
//! Job Object containment and the newline-delimited-JSON process shape used
//! by the chat sessions.

use std::ffi::c_void;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::windows::io::AsRawHandle as _;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};
use std::{io, mem, ptr, thread};

use serde_json::Value;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};

/// A Windows Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: every
/// process assigned to it (and each of their descendants) is terminated when
/// the last handle closes, i.e. when this value drops. This is the containment
/// that makes killing npm `.cmd` shims safe — killing only the shim would
/// strand the Node descendant that actually holds the inherited pipes.
pub(crate) struct KillOnCloseJob(HANDLE);

// A Job Object handle has no thread affinity. Ownership remains unique and
// Drop closes it exactly once, so moving the owner to a shutdown worker is
// equivalent to moving any other owned Windows kernel handle.
unsafe impl Send for KillOnCloseJob {}

impl KillOnCloseJob {
    pub(crate) fn attach(child: &Child) -> Result<Self, String> {
        unsafe {
            let job = CreateJobObjectW(ptr::null(), ptr::null());
            if job.is_null() {
                return Err(format!(
                    "failed to create process containment job: {}",
                    io::Error::last_os_error()
                ));
            }

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &raw const info as *const c_void,
                mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                let error = io::Error::last_os_error();
                CloseHandle(job);
                return Err(format!("failed to configure process containment: {error}"));
            }
            if AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) == 0 {
                let error = io::Error::last_os_error();
                CloseHandle(job);
                return Err(format!("failed to contain child process tree: {error}"));
            }
            Ok(Self(job))
        }
    }

    /// Attach with rollback: a child that cannot be contained is killed and
    /// reaped before the error propagates, because a running-but-uncontained
    /// process would escape every later job-based cleanup path.
    pub(crate) fn attach_or_kill(child: &mut Child) -> Result<Self, String> {
        Self::attach(child).map_err(|error| {
            let _ = child.kill();
            let _ = child.wait();
            error
        })
    }
}

impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// A spawned agent CLI with piped stdio, kill-on-close containment, and
/// newline-delimited JSON output. Stdout lines that parse as JSON are handed
/// to `deliver`, stderr lines to `on_stderr`, each from its own reader thread.
/// Dropping `deliver` signals EOF: the closure is owned by the reader thread
/// and dropped when the pipe closes.
pub(crate) struct JsonLineProcess {
    child: Child,
    /// Held until the root exits or forced shutdown terminates any remaining
    /// descendants.
    job: Option<KillOnCloseJob>,
    stdin: Option<ChildStdin>,
    /// Provider display name ("Codex", "Claude") for lifecycle error messages.
    provider: &'static str,
}

impl JsonLineProcess {
    /// Spawn `command` with all three stdio streams piped and start the two
    /// reader threads. `display_command` is the human-readable command line
    /// quoted in the spawn error.
    pub(crate) fn spawn(
        mut command: Command,
        display_command: &str,
        provider: &'static str,
        deliver: impl Fn(Value) + Send + 'static,
        on_stderr: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command
            .spawn()
            .map_err(|err| format!("could not run `{display_command}`: {err}"))?;
        let job = KillOnCloseJob::attach_or_kill(&mut child)?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("{provider} stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("{provider} stdout unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| format!("{provider} stderr unavailable"))?;

        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(message) = serde_json::from_str::<Value>(&line) {
                    deliver(message);
                }
            }
        });

        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                on_stderr(line);
            }
        });

        Ok(Self {
            child,
            job: Some(job),
            stdin: Some(stdin),
            provider,
        })
    }

    /// Write one protocol line. Failures are not surfaced here: a dead process
    /// also closes its stdout, so the reader-side EOF is the single
    /// exit-detection path.
    pub(crate) fn write_line(&mut self, message: &Value) {
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = writeln!(stdin, "{message}").and_then(|_| stdin.flush());
        }
    }

    /// False once shutdown has closed the protocol input.
    pub(crate) fn has_stdin(&self) -> bool {
        self.stdin.is_some()
    }

    /// Close the protocol input (EOF is the CLIs' graceful-shutdown signal)
    /// and wait for the process to exit. Forced termination is opt-in because
    /// it can interrupt an active tool operation; dropping the Job Object
    /// affects only this process's tree.
    pub(crate) fn shutdown(&mut self, timeout: Duration, force: bool) -> Result<(), String> {
        drop(self.stdin.take());
        let started = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.job.take();
                    return Ok(());
                }
                Ok(None) if started.elapsed() < timeout => {
                    thread::sleep(Duration::from_millis(20));
                }
                Ok(None) if force => {
                    self.job.take();
                    self.child.wait().map_err(|error| {
                        format!("could not wait for {} to stop: {error}", self.provider)
                    })?;
                    return Ok(());
                }
                Ok(None) => {
                    return Err(format!(
                        "{} did not stop before the update timeout",
                        self.provider
                    ));
                }
                Err(error) => {
                    return Err(format!(
                        "could not observe {} process exit: {error}",
                        self.provider
                    ));
                }
            }
        }
    }
}

impl Drop for JsonLineProcess {
    fn drop(&mut self) {
        // Closing stdin delivers EOF, which both CLIs treat as shutdown, and
        // the reader threads exit with their pipes. The forced fallback drops
        // the Job Object, which terminates the npm shim and its descendant
        // together instead of stranding the descendant.
        let _ = self.shutdown(Duration::from_millis(250), true);
    }
}
