//! The sessions a host offers to paired devices: the terminal and agent
//! tabs open on the host and the headless terminals remote devices started.
//! Sessions outlive the channels and views attached to them.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use nmt_config::{CursorShape, active_colors};
use nmt_platform::{PtyOptions, WinsizeBuilder, create_pty_with_env, default_shell};
use nmt_remote_core::rpc::{EndReason, Origin, SessionInfo, SessionKind, SessionWorkspace};
use nmt_terminal::event::{EventListener, Msg, MsgSender, TerminalEvent};
use nmt_terminal::session::TerminalSessionConfig;
use nmt_terminal::termio::{SessionOptions, SessionWorker, start_session};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::mpsc::{Sender, UnboundedSender};
use tokio::sync::{oneshot, watch};
use tracing::info;

/// What the host service needs to drive one session from a runtime thread,
/// without passing through the UI thread that owns a tab.
#[derive(Clone)]
pub struct TerminalControl {
    pub messenger: MsgSender,

    /// Set when a remote view resized the PTY. The host tab showing the
    /// session takes the size back the next time its user types, so the
    /// view in active use always sees the PTY at its own size.
    pub claimed_remotely: Arc<AtomicBool>,
}

/// What the host service needs to reach an agent session. The session
/// lives on the UI thread, so everything goes through requests it answers
/// there; its views and commands are opaque JSON here.
#[derive(Clone)]
pub struct AgentControl {
    pub requests: UnboundedSender<AgentRequest>,
}

pub enum AgentRequest {
    /// Open a view: reply with a snapshot, then send changes on `updates`
    /// until it closes.
    Attach {
        updates: UnboundedSender<Value>,
        reply: oneshot::Sender<Value>,
    },
    /// Open a bounded transcript view with independent history position.
    AttachPaged {
        view_id: String,
        updates: Sender<Value>,
        reply: oneshot::Sender<Value>,
    },
    /// Prepend one history page to the identified view.
    Page {
        view_id: String,
        cursor: Value,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    Call {
        method: String,
        params: Value,
        reply: oneshot::Sender<Result<Value, String>>,
    },
}

/// What the host service needs from the application instead of from one
/// session: what a device may start, and starting it. Both are answered on
/// the UI thread, and their values are opaque JSON here.
pub enum HostRequest {
    Info {
        reply: oneshot::Sender<Result<Value, String>>,
    },
    OpenAgent {
        params: Value,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    /// Start a terminal tab in the workspace `params` names.
    OpenTerminalTab {
        params: Value,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    /// Close the host tab pane that shows `session`. The error explains a
    /// refusal to the device.
    CloseSession {
        session: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Rename the host tab that shows `session`. The error explains a
    /// refusal to the device.
    RenameSession {
        session: String,
        title: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Start the host tab still waiting to be started under `session`, so a
    /// device can attach to its terminal. The error explains a refusal.
    StartTab {
        session: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Make `name` this host's name, as if its user had set it in settings.
    /// The application stores it and hands it back through
    /// `HostService::set_device_name`. The error explains a refusal.
    RenameHost {
        name: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

struct AgentEntry {
    info: SessionInfo,
    control: AgentControl,
}

pub struct SessionRegistry {
    inner: Mutex<Inner>,
    changed: watch::Sender<u64>,

    /// Bumped when a paired device starts or stops viewing a session. Kept
    /// apart from `changed`, which paired devices hear about: who is
    /// watching is shown on the host only.
    viewers_changed: watch::Sender<u64>,

    /// Bumped when the person at the host uses it, which signals to the host
    /// that paired devices away from it are no longer with the person.
    local_use: watch::Sender<u64>,

    /// Drawn once per registry and put in every terminal id. Ids count up
    /// from one in every run of the host, so without it an id a device kept
    /// across a host restart would reach whichever terminal took its number,
    /// or start a pending tab nobody asked for.
    run: String,
}

#[derive(Default)]
struct Inner {
    sessions: BTreeMap<String, Entry>,
    agents: BTreeMap<String, AgentEntry>,

    /// Host terminal tabs listed before their shells start. A host restores
    /// only the tab in front and starts the rest when they are first shown,
    /// and paired devices list them all the same.
    pending: BTreeMap<String, SessionInfo>,

    next: u64,

    /// Paired devices viewing each session, one entry per open view, so a
    /// device with two views of one session stays listed until both close.
    viewers: BTreeMap<String, Vec<Viewer>>,

    /// Where host requests go, once the application answers them.
    host: Option<UnboundedSender<HostRequest>>,
}

/// A paired device viewing a session.
#[derive(Clone)]
pub(crate) struct Viewer {
    pub key: [u8; 32],
    pub name: String,

    /// Reaches the channel holding the device's views, to end them.
    pub kicks: UnboundedSender<Kick>,
}

/// The host ending a device's views of `session`.
pub(crate) struct Kick {
    pub session: String,
    pub reason: EndReason,
}

struct Entry {
    info: SessionInfo,
    control: TerminalControl,

    /// The PTY size, watched by every attached remote view.
    size: watch::Sender<(u16, u16)>,

    /// Held for headless sessions only; dropping it ends the shell. Tab
    /// sessions belong to the tab.
    _worker: Option<SessionWorker>,
}

/// Events of a headless terminal. Nobody on the host is looking at it, so
/// bell, clipboard, and notification requests are dropped; the shell ending
/// removes the session.
struct HeadlessEvents {
    registry: Weak<SessionRegistry>,
    session: String,
}

impl SessionRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner::default()),
            changed: watch::channel(0).0,
            viewers_changed: watch::channel(0).0,
            local_use: watch::channel(0).0,
            run: run_tag(),
        })
    }

    /// A new terminal id, `prefix` telling host tabs from headless ones.
    fn next_id(&self, prefix: char) -> String {
        let mut inner = self.inner.lock();

        inner.next += 1;

        format!("{prefix}{}-{}", self.run, inner.next)
    }

    /// Offer a host tab's terminal. Returns its session id.
    pub fn register_tab(
        &self,
        title: String,
        cols: u16,
        rows: u16,
        control: TerminalControl,
    ) -> String {
        self.insert(Origin::Tab, title, cols, rows, control, None)
    }

    /// List a host terminal tab whose shell has not started yet. Returns its
    /// session id, which the tab goes live under through `register_tab_as`.
    pub fn register_pending_tab(&self, title: String) -> String {
        let session = self.next_id('t');

        self.inner.lock().pending.insert(
            session.clone(),
            SessionInfo {
                session: session.clone(),
                title,
                origin: Origin::Tab,
                cols: 0,
                rows: 0,
                kind: SessionKind::Terminal,
                harness: None,
                workspace: None,
                pending: true,
            },
        );

        self.notify();

        session
    }

    /// Offer a host tab's terminal under `session`, the id it was listed by
    /// while it waited to be started. The tab keeps the workspace it was
    /// listed under, so devices do not see it move while the host renders.
    pub fn register_tab_as(
        &self,
        session: String,
        title: String,
        cols: u16,
        rows: u16,
        control: TerminalControl,
    ) {
        let mut entry = Entry::new(
            session.clone(),
            Origin::Tab,
            title,
            cols,
            rows,
            control,
            None,
        );

        // The pending listing and the live entry swap under one lock, so a
        // device listing or attaching meanwhile sees the session either
        // asleep or live with its workspace, never missing or ungrouped.
        let mut inner = self.inner.lock();

        entry.info.workspace = inner
            .pending
            .remove(&session)
            .and_then(|info| info.workspace);

        inner.sessions.insert(session, entry);

        drop(inner);

        self.notify();
    }

    /// Whether `session` is a host tab still waiting to be started.
    pub fn is_pending(&self, session: &str) -> bool {
        self.inner.lock().pending.contains_key(session)
    }

    /// Offer a host agent tab under `session`, which the tab keeps across
    /// restarts so a device following it reattaches to the restored tab.
    /// Registering an id again replaces its control, as when a tab offered
    /// before it was restored goes live. `harness` names the agent it runs.
    pub fn register_agent(
        &self,
        session: String,
        title: String,
        harness: String,
        control: AgentControl,
    ) {
        self.insert_agent(session, title, harness, control, false);
    }

    /// Offer a host agent tab under `session` while it waits to be restored,
    /// listed as asleep until `register_agent` replaces it with the live tab.
    pub fn register_pending_agent(
        &self,
        session: String,
        title: String,
        harness: String,
        control: AgentControl,
    ) {
        self.insert_agent(session, title, harness, control, true);
    }

    fn insert_agent(
        &self,
        session: String,
        title: String,
        harness: String,
        control: AgentControl,
        pending: bool,
    ) {
        let mut inner = self.inner.lock();

        inner.agents.insert(
            session.clone(),
            AgentEntry {
                info: SessionInfo {
                    session: session.clone(),
                    title,
                    origin: Origin::Tab,
                    cols: 0,
                    rows: 0,
                    kind: SessionKind::Agent,
                    harness: Some(harness),
                    workspace: None,
                    pending,
                },
                control,
            },
        );

        drop(inner);

        self.notify();
    }

    /// Answer host requests on `requests` from now on.
    pub fn serve_host_requests(&self, requests: UnboundedSender<HostRequest>) {
        self.inner.lock().host = Some(requests);
    }

    pub(crate) fn host_requests(&self) -> Option<UnboundedSender<HostRequest>> {
        self.inner.lock().host.clone()
    }

    pub fn agent(&self, session: &str) -> Option<AgentControl> {
        self.inner
            .lock()
            .agents
            .get(session)
            .map(|entry| entry.control.clone())
    }

    /// Withdraw a session the host closed. Devices viewing it hear that the
    /// host closed it.
    pub fn unregister(&self, session: &str) {
        self.remove(session, true);
    }

    fn remove(&self, session: &str, kick_viewers: bool) {
        let (terminal, agent, viewers) = {
            let mut inner = self.inner.lock();

            inner.pending.remove(session);

            (
                inner.sessions.remove(session),
                inner.agents.remove(session),
                inner.viewers.remove(session),
            )
        };

        let watched = viewers.is_some();

        if kick_viewers {
            kick(session, viewers.unwrap_or_default(), EndReason::Closed);
        }

        drop((terminal, agent));

        self.notify();

        if watched {
            self.viewers_changed.send_modify(|version| *version += 1);
        }
    }

    /// The person at the host takes `session` back: every device viewing it
    /// loses its view and hears why. The session keeps running.
    pub fn take_back(&self, session: &str) {
        self.note_local_use();

        let viewers = self.inner.lock().viewers.remove(session);

        let Some(viewers) = viewers else {
            return;
        };

        kick(session, viewers, EndReason::TakenBack);

        self.viewers_changed.send_modify(|version| *version += 1);
    }

    pub(crate) fn add_viewer(&self, session: &str, viewer: Viewer) {
        self.inner
            .lock()
            .viewers
            .entry(session.to_owned())
            .or_default()
            .push(viewer);

        self.viewers_changed.send_modify(|version| *version += 1);
    }

    /// One view of `session` by the device with `key` closed.
    pub(crate) fn remove_viewer(&self, session: &str, key: &[u8; 32]) {
        let mut inner = self.inner.lock();

        let Some(viewers) = inner.viewers.get_mut(session) else {
            return;
        };

        let Some(index) = viewers.iter().position(|viewer| &viewer.key == key) else {
            return;
        };

        viewers.remove(index);

        if viewers.is_empty() {
            inner.viewers.remove(session);
        }

        drop(inner);

        self.viewers_changed.send_modify(|version| *version += 1);
    }

    /// Names of the paired devices viewing `session`, each device once.
    pub fn viewers(&self, session: &str) -> Vec<String> {
        let inner = self.inner.lock();

        let Some(viewers) = inner.viewers.get(session) else {
            return Vec::new();
        };

        let mut seen: Vec<&[u8; 32]> = Vec::new();

        viewers
            .iter()
            .filter(|viewer| {
                let first = !seen.contains(&&viewer.key);

                seen.push(&viewer.key);

                first
            })
            .map(|viewer| viewer.name.clone())
            .collect()
    }

    pub fn subscribe_viewers(&self) -> watch::Receiver<u64> {
        self.viewers_changed.subscribe()
    }

    /// The person at the host is using it.
    pub fn note_local_use(&self) {
        self.local_use.send_modify(|version| *version += 1);
    }

    pub(crate) fn subscribe_local_use(&self) -> watch::Receiver<u64> {
        self.local_use.subscribe()
    }

    pub fn set_title(&self, session: &str, title: String) {
        self.update_info(session, |info| {
            if info.title == title {
                return false;
            }

            info.title = title;

            true
        });
    }

    /// Record the host workspace whose tab shows `session`, which paired
    /// devices group their session lists by.
    pub fn set_workspace(&self, session: &str, workspace: Option<SessionWorkspace>) {
        self.update_info(session, |info| {
            if info.workspace == workspace {
                return false;
            }

            info.workspace = workspace;

            true
        });
    }

    /// Change a listed session's info, telling paired devices when `change`
    /// reports it changed anything.
    fn update_info(&self, session: &str, change: impl FnOnce(&mut SessionInfo) -> bool) {
        let mut inner = self.inner.lock();

        let inner = &mut *inner;

        let info = if let Some(entry) = inner.sessions.get_mut(session) {
            &mut entry.info
        } else if let Some(entry) = inner.agents.get_mut(session) {
            &mut entry.info
        } else if let Some(info) = inner.pending.get_mut(session) {
            info
        } else {
            return;
        };

        if change(info) {
            self.notify();
        }
    }

    pub fn list(&self) -> Vec<SessionInfo> {
        let inner = self.inner.lock();

        inner
            .sessions
            .values()
            .map(|entry| entry.info.clone())
            .chain(inner.agents.values().map(|entry| entry.info.clone()))
            .chain(inner.pending.values().cloned())
            .collect()
    }

    /// Headless sessions, which the host user can end.
    pub fn remote_sessions(&self) -> Vec<SessionInfo> {
        self.list()
            .into_iter()
            .filter(|info| info.origin == Origin::Remote)
            .collect()
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// What a remote view needs to attach: the session's control handle and
    /// its PTY size, current and future.
    pub(crate) fn attach_info(
        &self,
        session: &str,
    ) -> Option<(TerminalControl, watch::Receiver<(u16, u16)>)> {
        self.inner
            .lock()
            .sessions
            .get(session)
            .map(|entry| (entry.control.clone(), entry.size.subscribe()))
    }

    /// A host tab resized its session's PTY, or took the size back from a
    /// remote view. Remote views hear about it.
    pub fn note_local_resize(&self, session: &str, cols: u16, rows: u16) {
        let mut inner = self.inner.lock();

        let Some(entry) = inner.sessions.get_mut(session) else {
            return;
        };

        entry.info.cols = cols;
        entry.info.rows = rows;

        entry
            .control
            .claimed_remotely
            .store(false, Ordering::Relaxed);

        entry.size.send_if_modified(|size| {
            let changed = *size != (cols, rows);

            *size = (cols, rows);

            changed
        });
    }

    pub fn origin(&self, session: &str) -> Option<Origin> {
        let inner = self.inner.lock();

        inner
            .sessions
            .get(session)
            .map(|entry| entry.info.origin)
            .or_else(|| inner.agents.get(session).map(|entry| entry.info.origin))
            .or_else(|| inner.pending.get(session).map(|info| info.origin))
    }

    /// Resize a session's PTY for a remote view.
    pub(crate) fn resize(&self, session: &str, cols: u16, rows: u16) -> bool {
        let mut inner = self.inner.lock();

        let Some(entry) = inner.sessions.get_mut(session) else {
            return false;
        };

        entry.info.cols = cols;
        entry.info.rows = rows;

        entry
            .control
            .claimed_remotely
            .store(true, Ordering::Relaxed);

        entry.size.send_replace((cols, rows));

        // The host engine derives its cell size from these pixels for image
        // placement; a remote view's real cell size is unknown here, so a
        // typical one stands in.
        entry
            .control
            .messenger
            .send(Msg::Resize(WinsizeBuilder {
                cols,
                rows,
                width: cols.saturating_mul(8),
                height: rows.saturating_mul(16),
            }))
            .is_ok()
    }

    /// End a headless session. Host tabs are refused: they belong to the
    /// person at the host.
    pub fn close_remote(&self, session: &str) -> bool {
        let mut inner = self.inner.lock();

        if !inner
            .sessions
            .get(session)
            .is_some_and(|entry| entry.info.origin == Origin::Remote)
        {
            return false;
        }

        let removed = inner.sessions.remove(session);
        let viewers = inner.viewers.remove(session).unwrap_or_default();

        drop(inner);

        info!(%session, "closing a remote terminal");

        kick(session, viewers, EndReason::Closed);

        drop(removed);

        self.notify();

        true
    }

    /// End every headless session, as the host stops.
    pub(crate) fn close_all_remote(&self) {
        let removed: Vec<_> = {
            let mut inner = self.inner.lock();

            let ids: Vec<_> = inner
                .sessions
                .iter()
                .filter(|(_, entry)| entry.info.origin == Origin::Remote)
                .map(|(id, _)| id.clone())
                .collect();

            for id in &ids {
                kick(
                    id,
                    inner.viewers.remove(id).unwrap_or_default(),
                    EndReason::Closed,
                );
            }

            ids.iter()
                .filter_map(|id| inner.sessions.remove(id))
                .collect()
        };

        drop(removed);

        self.notify();
    }

    /// Start a headless terminal for a remote device with the host's shell.
    pub(crate) fn open_headless(
        self: &Arc<Self>,
        shell: Option<String>,
        args: Vec<String>,
        cols: u16,
        rows: u16,
    ) -> Result<String> {
        let config = TerminalSessionConfig {
            shell,
            args,
            ..TerminalSessionConfig::default()
        }
        .with_shell_integration();

        let program = config.shell.clone().unwrap_or_else(default_shell);
        let home = dirs::home_dir().map(|home| home.to_string_lossy().into_owned());

        let pty = create_pty_with_env(PtyOptions {
            shell: &program,
            args: &config.args,
            working_directory: home.as_deref(),
            columns: cols,
            rows,
            environment_overrides: &config.environment_overrides,
            starting_title: None,
            bootstrap: config.bootstrap.as_deref(),
        })
        .map_err(|error| anyhow!("starting {program}: {error}"))?;

        let session = self.next_id('r');

        // The host engine answers terminal queries next to the PTY, so the
        // program gets exactly one reply without a network round trip.
        let handles = start_session(
            pty,
            HeadlessEvents {
                registry: Arc::downgrade(self),
                session: session.clone(),
            },
            SessionOptions {
                cols,
                rows,
                route_id: 0,
                colors: active_colors(),
                cursor_shape: CursorShape::Block,
                scrollback_lines: config.scrollback_lines,
                engine_blocks: config.engine_blocks,
                terminal_responses: true,
            },
        )
        .map_err(|error| anyhow!("starting the terminal engine: {error}"))?;

        let title = program
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&program)
            .to_owned();

        let control = TerminalControl {
            messenger: handles.messenger,
            claimed_remotely: Arc::new(AtomicBool::new(false)),
        };

        self.insert_with_id(
            session.clone(),
            Origin::Remote,
            title,
            cols,
            rows,
            control,
            Some(handles.worker),
        );

        info!(%session, cols, rows, "opened a remote terminal");

        Ok(session)
    }

    fn insert(
        &self,
        origin: Origin,
        title: String,
        cols: u16,
        rows: u16,
        control: TerminalControl,
        worker: Option<SessionWorker>,
    ) -> String {
        let session = self.next_id('t');

        self.insert_with_id(session.clone(), origin, title, cols, rows, control, worker);

        session
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_with_id(
        &self,
        session: String,
        origin: Origin,
        title: String,
        cols: u16,
        rows: u16,
        control: TerminalControl,
        worker: Option<SessionWorker>,
    ) {
        let entry = Entry::new(session.clone(), origin, title, cols, rows, control, worker);

        self.inner.lock().sessions.insert(session, entry);

        self.notify();
    }

    fn notify(&self) {
        self.changed.send_modify(|version| *version += 1);
    }
}

impl Entry {
    /// A live terminal listed outside any workspace.
    fn new(
        session: String,
        origin: Origin,
        title: String,
        cols: u16,
        rows: u16,
        control: TerminalControl,
        worker: Option<SessionWorker>,
    ) -> Self {
        Self {
            info: SessionInfo {
                session,
                title,
                origin,
                cols,
                rows,
                kind: SessionKind::Terminal,
                harness: None,
                workspace: None,
                pending: false,
            },
            control,
            size: watch::channel((cols, rows)).0,
            _worker: worker,
        }
    }
}

/// Eight hex digits from the system's random source, falling back to the
/// clock, which still differs between two runs of the host.
fn run_tag() -> String {
    let mut bytes = [0; 4];

    if getrandom::fill(&mut bytes).is_err() {
        bytes = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.subsec_nanos())
            .to_le_bytes();
    }

    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Tell each channel among `viewers` once that its views of `session` end.
fn kick(session: &str, viewers: Vec<Viewer>, reason: EndReason) {
    let mut told: Vec<UnboundedSender<Kick>> = Vec::new();

    for viewer in viewers {
        if told.iter().any(|sender| sender.same_channel(&viewer.kicks)) {
            continue;
        }

        let _ = viewer.kicks.send(Kick {
            session: session.to_owned(),
            reason,
        });

        told.push(viewer.kicks);
    }
}

impl EventListener for HeadlessEvents {
    fn send_event(&self, event: TerminalEvent) {
        // The shell ending is not the host closing the session: devices
        // see the exit in their own views, as they would for a local shell.
        if let TerminalEvent::CloseTerminal(_) = event
            && let Some(registry) = self.registry.upgrade()
        {
            registry.remove(&self.session, false);
        }
    }
}
