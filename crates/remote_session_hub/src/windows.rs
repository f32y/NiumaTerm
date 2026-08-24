use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::{error, fmt, io, thread};

use nmt_config::{CursorShape, active_colors};
use nmt_platform::windows::powershell::DEFAULT_SHELL;
use nmt_platform::windows::process::ProcessTree;
use nmt_platform::{WinsizeBuilder, create_managed_pty_with_env, create_pty_with_env};
use nmt_terminal::event::{EventListener, Msg, MsgSender, TerminalEvent, WindowId};
use nmt_terminal::ghostty::GhosttyTerminal;
use nmt_terminal::pty_pipe::{SessionOptions as PipeOptions, start_session};
use parking_lot::{FairMutex, Mutex};

const SUBSCRIBER_QUEUE_CAPACITY: usize = 128;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionId(pub u64);

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug)]
pub struct SessionOptions {
    pub shell: String,
    pub args: Vec<String>,
    pub working_directory: Option<String>,
    pub environment_overrides: Vec<(String, String)>,
    pub starting_title: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub scrollback_lines: usize,
    pub manage_process_tree: bool,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            shell: DEFAULT_SHELL.to_owned(),
            args: Vec::new(),
            working_directory: None,
            environment_overrides: Vec::new(),
            starting_title: None,
            cols: 80,
            rows: 24,
            scrollback_lines: 10_000,
            manage_process_tree: false,
        }
    }
}

#[derive(Debug)]
pub struct SessionSnapshot {
    pub session_id: SessionId,
    pub base_seq: u64,
    pub vt: Vec<u8>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Clone, Debug)]
pub enum SessionEvent {
    Output { seq: u64, data: Arc<[u8]> },
    Exited { seq: u64 },
}

impl SessionEvent {
    pub fn seq(&self) -> u64 {
        match self {
            Self::Output { seq, .. } | Self::Exited { seq } => *seq,
        }
    }
}

/// A detachable client view. Dropping it never stops the underlying shell.
pub struct SessionSubscription {
    snapshot: SessionSnapshot,
    receiver: Receiver<SessionEvent>,
    subscriber_id: u64,
    stream: Arc<Mutex<StreamState>>,
}

impl SessionSubscription {
    pub fn snapshot(&self) -> &SessionSnapshot {
        &self.snapshot
    }

    pub fn events(&self) -> &Receiver<SessionEvent> {
        &self.receiver
    }

    /// Register the thread to unpark when new events are published, so a host
    /// event loop can block on `thread::park` instead of polling `events()`.
    pub fn set_wake_thread(&self, thread: thread::Thread) {
        if let Some(subscriber) = self.stream.lock().subscribers.get_mut(&self.subscriber_id) {
            subscriber.wake_thread = Some(thread);
        }
    }
}

impl Drop for SessionSubscription {
    fn drop(&mut self) {
        self.stream.lock().subscribers.remove(&self.subscriber_id);
    }
}

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub id: SessionId,
    pub shell: String,
    pub title: Option<String>,
    pub exited: bool,
    pub attached_clients: usize,
}

#[derive(Debug)]
pub enum HubError {
    SessionNotFound(SessionId),
    SessionExited(SessionId),
    InvalidSize { cols: u16, rows: u16 },
    Spawn(io::Error),
    Engine(String),
    ChannelClosed(SessionId),
}

impl fmt::Display for HubError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionNotFound(id) => write!(formatter, "terminal session {id} was not found"),
            Self::SessionExited(id) => write!(formatter, "terminal session {id} has exited"),
            Self::InvalidSize { cols, rows } => {
                write!(
                    formatter,
                    "terminal size must be non-zero, got {cols}x{rows}"
                )
            }
            Self::Spawn(error) => write!(formatter, "failed to start ConPTY session: {error}"),
            Self::Engine(error) => write!(formatter, "terminal engine failed: {error}"),
            Self::ChannelClosed(id) => write!(formatter, "terminal session {id} is unavailable"),
        }
    }
}

impl error::Error for HubError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Self::Spawn(error) => Some(error),
            _ => None,
        }
    }
}

struct StreamState {
    next_seq: u64,
    next_subscriber_id: u64,
    exited: bool,
    subscribers: HashMap<u64, Subscriber>,
}

struct Subscriber {
    sender: SyncSender<SessionEvent>,
    wake_thread: Option<thread::Thread>,
}

impl Default for StreamState {
    fn default() -> Self {
        Self {
            next_seq: 1,
            next_subscriber_id: 1,
            exited: false,
            subscribers: HashMap::new(),
        }
    }
}

impl StreamState {
    fn publish_output(&mut self, data: Arc<[u8]>) {
        if self.exited {
            return;
        }

        let seq = self.take_seq();

        self.publish(SessionEvent::Output { seq, data });
    }

    fn publish_exit(&mut self) {
        if self.exited {
            return;
        }

        self.exited = true;

        let seq = self.take_seq();

        self.publish(SessionEvent::Exited { seq });
    }

    fn take_seq(&mut self) -> u64 {
        let seq = self.next_seq;

        self.next_seq = self.next_seq.saturating_add(1);

        seq
    }

    fn publish(&mut self, event: SessionEvent) {
        // A full queue means the client can no longer consume a lossless ordered
        // stream. Detaching it forces a later reconnect to start from a fresh VT
        // checkpoint instead of silently continuing with missing bytes.
        self.subscribers.retain(
            |_, subscriber| match subscriber.sender.try_send(event.clone()) {
                Ok(()) => {
                    if let Some(thread) = &subscriber.wake_thread {
                        thread.unpark();
                    }

                    true
                }
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
            },
        );
    }
}

#[derive(Clone)]
struct HubEventProxy {
    stream: Arc<Mutex<StreamState>>,
}

impl EventListener for HubEventProxy {
    fn event(&self) -> (Option<TerminalEvent>, bool) {
        (None, false)
    }

    fn send_event(&self, event: TerminalEvent, _id: WindowId) {
        if matches!(event, TerminalEvent::CloseTerminal(_) | TerminalEvent::Exit) {
            self.stream.lock().publish_exit();
        }
    }
}

struct RemoteSession {
    id: SessionId,
    shell: String,
    title: Option<String>,
    messenger: MsgSender,
    engine: Arc<FairMutex<GhosttyTerminal>>,
    stream: Arc<Mutex<StreamState>>,
    shutdown_sent: AtomicBool,
    process_tree: Option<ProcessTree>,
}

impl RemoteSession {
    fn send(&self, message: Msg) -> Result<(), HubError> {
        if self.stream.lock().exited {
            return Err(HubError::SessionExited(self.id));
        }

        self.messenger
            .send(message)
            .map_err(|_| HubError::ChannelClosed(self.id))
    }

    fn shutdown(&self) {
        if !self.shutdown_sent.swap(true, Ordering::AcqRel) {
            self.stream.lock().publish_exit();

            let _ = self.messenger.send(Msg::Shutdown);
        }
    }
}

impl Drop for RemoteSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Default)]
pub struct RemoteSessionHub {
    next_session_id: AtomicU64,
    sessions: Mutex<HashMap<SessionId, Arc<RemoteSession>>>,
}

impl RemoteSessionHub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(&self, options: SessionOptions) -> Result<SessionId, HubError> {
        validate_size(options.cols, options.rows)?;

        let id = SessionId(self.next_session_id.fetch_add(1, Ordering::Relaxed) + 1);
        let stream = Arc::new(Mutex::new(StreamState::default()));

        let pty = if options.manage_process_tree {
            create_managed_pty_with_env(
                &options.shell,
                options.args.clone(),
                &options.working_directory,
                options.cols,
                options.rows,
                &options.environment_overrides,
                options.starting_title.as_deref(),
            )
        } else {
            create_pty_with_env(
                &options.shell,
                options.args.clone(),
                &options.working_directory,
                options.cols,
                options.rows,
                &options.environment_overrides,
                options.starting_title.as_deref(),
            )
        }
        .map_err(HubError::Spawn)?;

        let process_tree = pty.process_tree();

        let output_stream = Arc::clone(&stream);

        let handles = start_session(
            pty,
            HubEventProxy {
                stream: Arc::clone(&stream),
            },
            PipeOptions {
                cols: options.cols,
                rows: options.rows,
                route_id: id.0 as usize,
                colors: active_colors(),
                cursor_shape: CursorShape::default(),
                scrollback_lines: options.scrollback_lines,
                engine_blocks: false,
                // The attached frontend owns terminal identity and theme, so it
                // must be the sole responder to DA/DSR/OSC queries. Replying
                // here as well sends duplicate device reports and exposes the
                // Hub process's default colors.
                terminal_responses: false,
                output_sink: Some(Arc::new(move |data| {
                    output_stream.lock().publish_output(data)
                })),
            },
        )
        .map_err(|error| HubError::Engine(error.to_string()))?;

        let session = Arc::new(RemoteSession {
            id,
            shell: options.shell,
            title: options.starting_title,
            messenger: handles.messenger,
            engine: handles.engine,
            stream,
            shutdown_sent: AtomicBool::new(false),
            process_tree,
        });

        self.sessions.lock().insert(id, session);

        Ok(id)
    }

    pub fn attach(&self, id: SessionId) -> Result<SessionSubscription, HubError> {
        let session = self.get(id)?;
        let (sender, receiver) = sync_channel(SUBSCRIBER_QUEUE_CAPACITY);

        // PTY output publishes its sequence while holding this same engine lock.
        // The checkpoint and subscription registration therefore form one stream
        // boundary: every byte is either in the checkpoint or in a later event.
        let mut engine = session.engine.lock();

        let vt = engine
            .format_vt_state()
            .map_err(|error| HubError::Engine(error.to_string()))?;

        let cols = engine.cols();
        let rows = engine.rows();

        let mut stream = session.stream.lock();

        if stream.exited {
            return Err(HubError::SessionExited(id));
        }

        let subscriber_id = stream.next_subscriber_id;

        stream.next_subscriber_id = stream.next_subscriber_id.saturating_add(1);

        let base_seq = stream.next_seq.saturating_sub(1);

        stream.subscribers.insert(
            subscriber_id,
            Subscriber {
                sender,
                wake_thread: None,
            },
        );

        drop(stream);
        drop(engine);

        Ok(SessionSubscription {
            snapshot: SessionSnapshot {
                session_id: id,
                base_seq,
                vt,
                cols,
                rows,
            },
            receiver,
            subscriber_id,
            stream: Arc::clone(&session.stream),
        })
    }

    pub fn write_input(&self, id: SessionId, data: &[u8]) -> Result<(), HubError> {
        if data.is_empty() {
            return Ok(());
        }

        self.get(id)?.send(Msg::Input(Cow::Owned(data.to_vec())))
    }

    pub fn resize(&self, id: SessionId, cols: u16, rows: u16) -> Result<(), HubError> {
        validate_size(cols, rows)?;

        self.get(id)?.send(Msg::Resize(WinsizeBuilder {
            cols,
            rows,
            width: cols,
            height: rows,
        }))
    }

    pub fn kill(&self, id: SessionId) -> Result<(), HubError> {
        let session = self
            .sessions
            .lock()
            .remove(&id)
            .ok_or(HubError::SessionNotFound(id))?;

        session.shutdown();

        Ok(())
    }

    pub fn child_process_count(&self, id: SessionId) -> Result<usize, HubError> {
        Ok(self
            .get(id)?
            .process_tree
            .as_ref()
            .map_or(0, ProcessTree::other_process_count))
    }

    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        self.sessions
            .lock()
            .values()
            .map(|session| {
                let stream = session.stream.lock();
                SessionInfo {
                    id: session.id,
                    shell: session.shell.clone(),
                    title: session.title.clone(),
                    exited: stream.exited,
                    attached_clients: stream.subscribers.len(),
                }
            })
            .collect()
    }

    fn get(&self, id: SessionId) -> Result<Arc<RemoteSession>, HubError> {
        self.sessions
            .lock()
            .get(&id)
            .cloned()
            .ok_or(HubError::SessionNotFound(id))
    }
}

fn validate_size(cols: u16, rows: u16) -> Result<(), HubError> {
    if cols == 0 || rows == 0 {
        Err(HubError::InvalidSize { cols, rows })
    } else {
        Ok(())
    }
}
