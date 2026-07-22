use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::{self, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, OnceLock, Weak};
use std::thread::Thread;
use std::time::{Duration, Instant};

use nmt_platform::{
    ChildEvent, EventedPty, Interest, Poll, ProcessReadWrite, SoftReady, Token, Waker,
    WinsizeBuilder,
};
use parking_lot::{Condvar, Mutex};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::ipc::{
    DEFAULT_MAILBOX_CAPACITY, HubRequest, HubResponse, IpcError, SharedMemoryEndpoint,
};
use crate::{SessionId, SessionOptions};

const IPC_SEND_TIMEOUT: Duration = Duration::from_secs(5);
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

static DEFAULT_CLIENT: OnceLock<Mutex<DefaultClient>> = OnceLock::new();

#[derive(Default)]
struct DefaultClient {
    client: Weak<HubClient>,
    exit: Weak<ProcessExit>,
}

#[derive(Debug, Clone)]
pub struct HubClientError(String);

impl HubClientError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for HubClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for HubClientError {}

/// One process-wide connection to `SessionHub.exe`. Construction is lazy, so
/// merely linking this crate creates no child process, mapping, or worker thread.
pub struct HubClient {
    commands: mpsc::Sender<ClientCommand>,
    worker: Thread,
    exit: Arc<ProcessExit>,
    shutdown_sent: AtomicBool,
}

#[derive(Default)]
struct ProcessExit {
    exited: Mutex<bool>,
    changed: Condvar,
}

impl ProcessExit {
    fn mark_exited(&self) {
        *self.exited.lock() = true;
        self.changed.notify_all();
    }

    fn wait(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;

        let mut exited = self.exited.lock();

        while !*exited {
            let remaining = deadline.saturating_duration_since(Instant::now());

            if remaining.is_zero() {
                return false;
            }

            self.changed.wait_for(&mut exited, remaining);
        }
        true
    }
}

impl HubClient {
    pub fn connect_default() -> Result<Arc<Self>, HubClientError> {
        let slot = DEFAULT_CLIENT.get_or_init(|| Mutex::new(DefaultClient::default()));

        let mut slot = slot.lock();

        if let Some(client) = slot.client.upgrade() {
            return Ok(client);
        }

        let executable = default_hub_executable()?;

        let client = Self::spawn(&executable)?;

        slot.client = Arc::downgrade(&client);
        slot.exit = Arc::downgrade(&client.exit);

        Ok(client)
    }

    pub fn spawn(executable: &Path) -> Result<Arc<Self>, HubClientError> {
        let (commands, command_rx) = mpsc::channel();
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        let executable = executable.to_owned();
        let exit = Arc::new(ProcessExit::default());
        let client_exit = Arc::clone(&exit);

        let handle = std::thread::Builder::new()
            .name("session-hub-client".to_owned())
            .spawn(move || client_loop(&executable, command_rx, startup_tx, client_exit))
            .map_err(|error| HubClientError::new(format!("failed to start Hub client: {error}")))?;

        let worker = handle.thread().clone();

        // Dropping the JoinHandle detaches the pump. The command channel and Hub
        // process own its actual lifetime, avoiding a UI-thread join during teardown.
        drop(handle);

        startup_rx.recv_timeout(OPEN_TIMEOUT).map_err(|error| {
            HubClientError::new(format!("SessionHub startup timed out: {error}"))
        })??;

        Ok(Arc::new(Self {
            commands,
            worker,
            exit,
            shutdown_sent: AtomicBool::new(false),
        }))
    }

    pub fn open(self: &Arc<Self>, options: SessionOptions) -> Result<RemotePty, HubClientError> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);

        self.send(ClientCommand::Open {
            options,
            reply: reply_tx,
        })?;

        let (session_id, state) = reply_rx.recv_timeout(OPEN_TIMEOUT).map_err(|error| {
            HubClientError::new(format!("SessionHub open timed out: {error}"))
        })??;

        Ok(RemotePty::new(session_id, state, Arc::clone(self)))
    }

    fn send(&self, command: ClientCommand) -> Result<(), HubClientError> {
        self.commands
            .send(command)
            .map_err(|_| HubClientError::new("SessionHub client is not running"))?;

        self.worker.unpark();

        Ok(())
    }

    fn child_process_count(&self, session_id: SessionId) -> Result<usize, HubClientError> {
        let (reply, result) = mpsc::sync_channel(1);

        self.send(ClientCommand::ChildProcessCount { session_id, reply })?;

        result.recv_timeout(REQUEST_TIMEOUT).map_err(|error| {
            HubClientError::new(format!("SessionHub process count timed out: {error}"))
        })?
    }

    pub fn shutdown(&self) -> Result<(), HubClientError> {
        if !self.shutdown_sent.swap(true, Ordering::AcqRel) {
            self.commands
                .send(ClientCommand::Shutdown)
                .map_err(|_| HubClientError::new("SessionHub client is not running"))?;
            self.worker.unpark();
        }

        if self.exit.wait(SHUTDOWN_TIMEOUT) {
            Ok(())
        } else {
            Err(HubClientError::new(
                "SessionHub did not exit within 5 seconds",
            ))
        }
    }
}

impl Drop for HubClient {
    fn drop(&mut self) {
        if !self.shutdown_sent.swap(true, Ordering::AcqRel) {
            let _ = self.commands.send(ClientCommand::Shutdown);
            self.worker.unpark();
        }
    }
}

/// Stop the lazily-created process-wide Hub and wait for its process to exit.
/// A missing Hub means remote sessions were never enabled and is a no-op.
pub fn shutdown_default() -> Result<(), HubClientError> {
    let Some(slot) = DEFAULT_CLIENT.get() else {
        return Ok(());
    };

    let (client, exit) = {
        let slot = slot.lock();
        (slot.client.upgrade(), slot.exit.upgrade())
    };

    if let Some(client) = client {
        client.shutdown()
    } else if let Some(exit) = exit {
        if exit.wait(SHUTDOWN_TIMEOUT) {
            Ok(())
        } else {
            Err(HubClientError::new(
                "SessionHub did not exit within 5 seconds",
            ))
        }
    } else {
        Ok(())
    }
}

fn default_hub_executable() -> Result<PathBuf, HubClientError> {
    let current = std::env::current_exe().map_err(|error| {
        HubClientError::new(format!("cannot locate NiumaTerm executable: {error}"))
    })?;

    Ok(current.with_file_name("SessionHub.exe"))
}

enum ClientCommand {
    Open {
        options: SessionOptions,
        reply: SyncSender<Result<(SessionId, Arc<SessionState>), HubClientError>>,
    },
    Input {
        session_id: SessionId,
        data: Vec<u8>,
    },
    Resize {
        session_id: SessionId,
        cols: u16,
        rows: u16,
    },
    Kill {
        session_id: SessionId,
    },
    ChildProcessCount {
        session_id: SessionId,
        reply: SyncSender<Result<usize, HubClientError>>,
    },
    Shutdown,
}

enum PendingRequest {
    Open {
        state: Arc<SessionState>,
        reply: SyncSender<Result<(SessionId, Arc<SessionState>), HubClientError>>,
    },
    Attach {
        session_id: SessionId,
        state: Arc<SessionState>,
        reply: SyncSender<Result<(SessionId, Arc<SessionState>), HubClientError>>,
    },
    ChildProcessCount {
        reply: SyncSender<Result<usize, HubClientError>>,
    },
}

enum ReaderEvent {
    Message(Vec<u8>),
    Failed(String),
    ChildExited,
}

fn client_loop(
    executable: &Path,
    commands: Receiver<ClientCommand>,
    startup: SyncSender<Result<(), HubClientError>>,
    exit: Arc<ProcessExit>,
) {
    if let Err(error) = run_client_loop(executable, commands, &startup, exit) {
        let _ = startup.send(Err(error));
    }
}

fn run_client_loop(
    executable: &Path,
    commands: Receiver<ClientCommand>,
    startup: &SyncSender<Result<(), HubClientError>>,
    exit: Arc<ProcessExit>,
) -> Result<(), HubClientError> {
    let mut endpoint =
        SharedMemoryEndpoint::create_parent(DEFAULT_MAILBOX_CAPACITY).map_err(client_ipc_error)?;

    let os_id = endpoint.os_id().to_owned();

    let mut child = Command::new(executable)
        .arg(&os_id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|error| {
            HubClientError::new(format!(
                "failed to start '{}': {error}",
                executable.display()
            ))
        })?;

    let (events_tx, events_rx) = mpsc::channel();
    let pump = std::thread::current();

    spawn_response_reader(&os_id, events_tx.clone(), pump.clone())?;

    std::thread::Builder::new()
        .name("session-hub-process-wait".to_owned())
        .spawn(move || {
            let _ = child.wait();
            exit.mark_exited();
            let _ = events_tx.send(ReaderEvent::ChildExited);
            pump.unpark();
        })
        .map_err(|error| HubClientError::new(format!("failed to watch SessionHub: {error}")))?;

    startup
        .send(Ok(()))
        .map_err(|_| HubClientError::new("SessionHub startup receiver was dropped"))?;

    let mut next_request_id = 1_u64;
    let mut pending = HashMap::<u64, PendingRequest>::new();
    let mut sessions = HashMap::<SessionId, Arc<SessionState>>::new();

    loop {
        let mut did_work = false;
        while let Ok(command) = commands.try_recv() {
            did_work = true;
            match command {
                ClientCommand::Open { options, reply } => {
                    let request_id = take_request_id(&mut next_request_id);
                    let state = Arc::new(SessionState::default());

                    send_request(
                        &mut endpoint,
                        HubRequest::Open {
                            request_id,
                            options,
                        },
                    )?;

                    pending.insert(request_id, PendingRequest::Open { state, reply });
                }
                ClientCommand::Input { session_id, data } => {
                    send_request(&mut endpoint, HubRequest::Input { session_id, data })?
                }
                ClientCommand::Resize {
                    session_id,
                    cols,
                    rows,
                } => send_request(
                    &mut endpoint,
                    HubRequest::Resize {
                        session_id,
                        cols,
                        rows,
                    },
                )?,
                ClientCommand::Kill { session_id } => {
                    sessions.remove(&session_id);

                    send_request(
                        &mut endpoint,
                        HubRequest::Kill {
                            request_id: take_request_id(&mut next_request_id),
                            session_id,
                        },
                    )?;
                }
                ClientCommand::ChildProcessCount { session_id, reply } => {
                    let request_id = take_request_id(&mut next_request_id);

                    send_request(
                        &mut endpoint,
                        HubRequest::ChildProcessCount {
                            request_id,
                            session_id,
                        },
                    )?;

                    pending.insert(request_id, PendingRequest::ChildProcessCount { reply });
                }
                ClientCommand::Shutdown => {
                    let _ = send_request(&mut endpoint, HubRequest::Shutdown);

                    return Ok(());
                }
            }
        }

        while let Ok(event) = events_rx.try_recv() {
            did_work = true;
            match event {
                ReaderEvent::Message(bytes) => handle_response(
                    HubResponse::decode(&bytes).map_err(client_ipc_error)?,
                    &mut endpoint,
                    &mut next_request_id,
                    &mut pending,
                    &mut sessions,
                )?,
                ReaderEvent::Failed(message) => {
                    fail_all(&mut pending, &sessions, &message);

                    return Err(HubClientError::new(message));
                }
                ReaderEvent::ChildExited => {
                    let message = "SessionHub exited unexpectedly";

                    fail_all(&mut pending, &sessions, message);

                    return Err(HubClientError::new(message));
                }
            }
        }

        if !did_work {
            std::thread::park();
        }
    }
}

fn spawn_response_reader(
    os_id: &str,
    sender: mpsc::Sender<ReaderEvent>,
    pump: Thread,
) -> Result<(), HubClientError> {
    let os_id = os_id.to_owned();
    std::thread::Builder::new()
        .name("session-hub-response-reader".to_owned())
        .spawn(move || {
            let result = (|| -> Result<(), IpcError> {
                let mut endpoint = SharedMemoryEndpoint::open_parent(&os_id)?;

                loop {
                    let message = endpoint.recv_blocking()?;

                    if sender.send(ReaderEvent::Message(message)).is_err() {
                        return Ok(());
                    }

                    pump.unpark();
                }
            })();

            if let Err(error) = result {
                let _ = sender.send(ReaderEvent::Failed(error.to_string()));

                pump.unpark();
            }
        })
        .map_err(|error| HubClientError::new(format!("failed to read SessionHub IPC: {error}")))?;
    Ok(())
}

fn handle_response(
    response: HubResponse,
    endpoint: &mut SharedMemoryEndpoint,
    next_request_id: &mut u64,
    pending: &mut HashMap<u64, PendingRequest>,
    sessions: &mut HashMap<SessionId, Arc<SessionState>>,
) -> Result<(), HubClientError> {
    match response {
        HubResponse::Opened {
            request_id,
            session_id,
        } => {
            let Some(PendingRequest::Open { state, reply }) = pending.remove(&request_id) else {
                return Ok(());
            };

            let attach_id = take_request_id(next_request_id);

            send_request(
                endpoint,
                HubRequest::Attach {
                    request_id: attach_id,
                    session_id,
                },
            )?;

            pending.insert(
                attach_id,
                PendingRequest::Attach {
                    session_id,
                    state,
                    reply,
                },
            );
        }
        HubResponse::Snapshot {
            request_id,
            session_id,
            base_seq,
            vt,
            ..
        } => {
            let Some(PendingRequest::Attach {
                session_id: expected_id,
                state,
                reply,
            }) = pending.remove(&request_id)
            else {
                return Ok(());
            };

            if session_id != expected_id {
                let _ = reply.send(Err(HubClientError::new(
                    "SessionHub returned a snapshot for the wrong session",
                )));
                return Ok(());
            }

            state.next_seq.store(base_seq + 1, Ordering::Release);

            state.push_output(vt);

            sessions.insert(session_id, Arc::clone(&state));

            let _ = reply.send(Ok((session_id, state)));
        }
        HubResponse::Output {
            session_id,
            seq,
            data,
        } => {
            if let Some(state) = sessions.get(&session_id) {
                let expected = state.next_seq.load(Ordering::Acquire);

                if seq == expected {
                    state.next_seq.store(expected + 1, Ordering::Release);

                    state.push_output(data);
                } else if seq > expected {
                    // A sequence gap means bytes were lost before the local VT parser.
                    // Continuing would leave its screen state silently corrupted.
                    state.mark_exited();
                }
            }
        }
        HubResponse::Exited { session_id, .. } => {
            if let Some(state) = sessions.get(&session_id) {
                state.mark_exited();
            }
        }
        HubResponse::ChildProcessCount { request_id, count } => {
            let Some(PendingRequest::ChildProcessCount { reply }) = pending.remove(&request_id)
            else {
                return Ok(());
            };

            let _ = reply.send(Ok(count.try_into().unwrap_or(usize::MAX)));
        }
        HubResponse::Error {
            request_id,
            message,
        } => {
            if let Some(request) = pending.remove(&request_id) {
                match request {
                    PendingRequest::Open { reply, .. } | PendingRequest::Attach { reply, .. } => {
                        let _ = reply.send(Err(HubClientError::new(message)));
                    }
                    PendingRequest::ChildProcessCount { reply } => {
                        let _ = reply.send(Err(HubClientError::new(message)));
                    }
                }
            }
        }
        HubResponse::Ack { .. } => {}
    }
    Ok(())
}

fn fail_all(
    pending: &mut HashMap<u64, PendingRequest>,
    sessions: &HashMap<SessionId, Arc<SessionState>>,
    message: &str,
) {
    for (_, request) in pending.drain() {
        match request {
            PendingRequest::Open { reply, .. } | PendingRequest::Attach { reply, .. } => {
                let _ = reply.send(Err(HubClientError::new(message)));
            }
            PendingRequest::ChildProcessCount { reply } => {
                let _ = reply.send(Err(HubClientError::new(message)));
            }
        }
    }

    for state in sessions.values() {
        state.mark_exited();
    }
}

fn send_request(
    endpoint: &mut SharedMemoryEndpoint,
    request: HubRequest,
) -> Result<(), HubClientError> {
    let bytes = request.encode().map_err(client_ipc_error)?;

    endpoint
        .send(&bytes, IPC_SEND_TIMEOUT)
        .map_err(client_ipc_error)
}

fn client_ipc_error(error: IpcError) -> HubClientError {
    HubClientError::new(format!("SessionHub IPC failed: {error}"))
}

fn take_request_id(next: &mut u64) -> u64 {
    let id = *next;

    *next = (*next).wrapping_add(1).max(1);

    id
}

#[derive(Default)]
struct SessionState {
    output: Mutex<VecDeque<u8>>,
    read_ready: SoftReady,
    child_ready: SoftReady,
    exited: AtomicBool,
    exit_delivered: AtomicBool,
    next_seq: AtomicU64,
}

impl SessionState {
    fn push_output(&self, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }

        self.output.lock().extend(data);

        self.read_ready.set_ready();
    }

    fn mark_exited(&self) {
        self.exited.store(true, Ordering::Release);

        self.child_ready.set_ready();
    }
}

pub struct RemotePty {
    session_id: SessionId,
    state: Arc<SessionState>,
    client: Arc<HubClient>,
    reader: RemoteReader,
    writer: RemoteWriter,
    read_token: Token,
    write_token: Token,
    child_token: Token,
}

impl RemotePty {
    fn new(session_id: SessionId, state: Arc<SessionState>, client: Arc<HubClient>) -> Self {
        Self {
            session_id,
            reader: RemoteReader(Arc::clone(&state)),
            writer: RemoteWriter {
                session_id,
                client: Arc::clone(&client),
            },
            state,
            client,
            read_token: Token(0),
            write_token: Token(0),
            child_token: Token(0),
        }
    }

    pub fn spawn(mut options: SessionOptions) -> Result<Self, HubClientError> {
        options.manage_process_tree = nmt_platform::job_management_enabled();

        HubClient::connect_default()?.open(options)
    }

    pub fn control(&self) -> RemoteSessionControl {
        RemoteSessionControl {
            session_id: self.session_id,
            client: Arc::clone(&self.client),
        }
    }
}

#[derive(Clone)]
pub struct RemoteSessionControl {
    session_id: SessionId,
    client: Arc<HubClient>,
}

impl RemoteSessionControl {
    pub fn child_process_count(&self) -> Result<usize, HubClientError> {
        self.client.child_process_count(self.session_id)
    }
}

impl Drop for RemotePty {
    fn drop(&mut self) {
        let _ = self.client.send(ClientCommand::Kill {
            session_id: self.session_id,
        });
    }
}

pub struct RemoteReader(Arc<SessionState>);

impl Read for RemoteReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }

        let mut output = self.0.output.lock();

        if output.is_empty() {
            self.0.read_ready.clear();

            return if self.0.exited.load(Ordering::Acquire) {
                Ok(0)
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            };
        }

        let count = buffer.len().min(output.len());

        for byte in &mut buffer[..count] {
            *byte = output.pop_front().expect("output length was checked");
        }

        if output.is_empty() {
            self.0.read_ready.clear();
        }

        Ok(count)
    }
}

pub struct RemoteWriter {
    session_id: SessionId,
    client: Arc<HubClient>,
}

impl Write for RemoteWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }

        self.client
            .send(ClientCommand::Input {
                session_id: self.session_id,
                data: buffer.to_vec(),
            })
            .map_err(io::Error::other)?;

        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl ProcessReadWrite for RemotePty {
    type Reader = RemoteReader;

    type Writer = RemoteWriter;

    fn reader(&mut self) -> &mut Self::Reader {
        &mut self.reader
    }

    fn read_token(&self) -> Token {
        self.read_token
    }

    fn writer(&mut self) -> &mut Self::Writer {
        &mut self.writer
    }

    fn write_token(&self) -> Token {
        self.write_token
    }

    fn set_winsize(&mut self, size: WinsizeBuilder) -> io::Result<()> {
        self.client
            .send(ClientCommand::Resize {
                session_id: self.session_id,
                cols: size.cols,
                rows: size.rows,
            })
            .map_err(io::Error::other)
    }

    fn register(
        &mut self,
        _poll: &Poll,
        tokens: &mut dyn Iterator<Item = Token>,
        _interest: Interest,
        waker: &Arc<Waker>,
    ) -> io::Result<()> {
        self.read_token = tokens.next().expect("PtyPipe supplies a read token");
        self.write_token = tokens.next().expect("PtyPipe supplies a write token");
        self.child_token = tokens.next().expect("PtyPipe supplies a child token");

        self.state.read_ready.set_waker(Arc::clone(waker));
        self.state.child_ready.set_waker(Arc::clone(waker));

        Ok(())
    }

    fn reregister(&mut self, _poll: &Poll, _interest: Interest) -> io::Result<()> {
        Ok(())
    }

    fn deregister(&mut self, _poll: &Poll) -> io::Result<()> {
        Ok(())
    }

    fn drain_ready(&self) -> Vec<Token> {
        let mut ready = Vec::with_capacity(3);

        if self.state.read_ready.is_ready() {
            ready.push(self.read_token);
        }

        // Hub input is accepted into an unbounded in-process command queue, so it
        // is writable whenever PtyPipe wakes for a pending input message.
        ready.push(self.write_token);

        if self.state.child_ready.is_ready() {
            ready.push(self.child_token);
        }

        ready
    }

    fn has_ready(&self) -> bool {
        self.state.read_ready.is_ready() || self.state.child_ready.is_ready()
    }
}

impl EventedPty for RemotePty {
    fn child_event_token(&self) -> Token {
        self.child_token
    }

    fn next_child_event(&mut self) -> Option<ChildEvent> {
        if self.state.exited.load(Ordering::Acquire)
            && !self.state.exit_delivered.swap(true, Ordering::AcqRel)
        {
            self.state.child_ready.clear();

            Some(ChildEvent::Exited)
        } else {
            None
        }
    }
}
