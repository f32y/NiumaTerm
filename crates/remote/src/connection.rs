//! A client's lasting connection to one paired host.
//!
//! Sessions belong to the host and survive network loss, so the client
//! keeps its views and reconnects under them: after a drop it backs off
//! from 0.5 s to 30 s with jitter, and on success reattaches every open view
//! by session id. A caller that would rather tell the user than wait can
//! limit the attempts instead (see [`Retry`]). A reattached view starts
//! over from a checkpoint, which is
//! why views are addressed by session instead of by stream: stream ids
//! last only as long as one channel.
//!
//! A link through the relay keeps trying the host's LAN addresses in the
//! background, and moves to the LAN once one answers: a phone paired while
//! away goes direct when it comes home, without waiting for a reconnect. It
//! also tries a direct path through both NATs, signaled over the relay link
//! itself, and moves to that when it opens.

use std::collections::HashMap;
use std::mem;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use futures::{FutureExt as _, TryFutureExt as _};
use getrandom::fill;
use nmt_platform::runtime;
use nmt_remote_core::channel::Channel;
use nmt_remote_core::direct::{DirectOffer, Side};
use nmt_remote_core::frame::{CONTROL_STREAM, Message as FrameMessage, kind};
use nmt_remote_core::identity::{DeviceId, DeviceKey};
use nmt_remote_core::push::{PUSH_REGISTER, PUSH_UNREGISTER, PushRegistration};
use nmt_remote_core::rpc::{
    self, AgentAttached, AgentCall, AgentOpen, AgentOps, Control, EndReason, ErrorCode, HostInfo,
    HostName, RpcError, SessionEnded, SessionInfo, SessionList, SessionRef, SessionRename,
    StreamRef, TerminalOpen, TerminalOpenTab, TerminalResize,
};
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::select;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::{Notify, oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::{Instant, sleep, sleep_until, timeout};
use tracing::{Instrument as _, info, info_span, warn};

use crate::client::{LinkPath, PathPolicy, Refused, establish, establish_direct, establish_lan};
use crate::link::{BoxSocket, Outbound, SendQueue, pump};
use crate::network_pty::NetworkPty;
use crate::store::PairedHost;
use crate::{direct, netwatch};

const MIN_BACKOFF: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// A link with no views closes after this long, so a host that is no longer
/// used stops holding a socket open.
const IDLE_CLOSE: Duration = Duration::from_secs(60);

/// How long a request waits for a link before failing.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// A link that came up through the relay tries the LAN this soon: the
/// connection race gives the LAN only a short head start, and a host that
/// answers a little slowly on it loses to the relay.
const FIRST_LAN_ATTEMPT: Duration = Duration::from_secs(10);

/// And again this often while it stays on the relay. A failed attempt costs
/// a few seconds of connecting in the background.
const LAN_RETRY: Duration = Duration::from_secs(2 * 60);

/// A LAN channel waits at most this long for the relay link's requests to
/// finish before replacing it. Moving would fail them, and the new channel
/// cannot idle much longer before the host's liveness probes give up on it.
const UPGRADE_WAIT: Duration = Duration::from_secs(10);

/// A link that came up through the relay tries a direct path this soon.
/// Gathering and punching take a few seconds and run beside the relay, so
/// there is no reason to wait longer.
const FIRST_DIRECT_ATTEMPT: Duration = Duration::from_secs(3);

/// And again this often while it stays on the relay. NAT kinds rarely
/// change on one network, so a failed attempt is unlikely to succeed sooner;
/// a network change retries at once.
const DIRECT_RETRY: Duration = Duration::from_secs(5 * 60);

/// The most one direct attempt may take: gathering, the offer's round trip
/// through the relay, the QUIC connection, and the channel handshake. On a
/// network that blocks most STUN servers, gathering walks the whole list on
/// both sides, a few seconds per batch; the relay link stays in use
/// meanwhile, so a slow attempt costs nothing.
const DIRECT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// No view needs the host; nothing is connected.
    Idle,
    Connecting,
    Connected,
    /// The link dropped; views wait while it comes back.
    Reconnecting,
    /// The host no longer trusts this device. Pairing again is the fix.
    Refused,
    /// Every attempt of a [`Retry::Limited`] round failed. The next request,
    /// view, [`RemoteHost::keep_connected`], or network change starts
    /// another round.
    Unreachable,
}

/// How a link that cannot reach its host keeps trying.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retry {
    /// With growing backoff until the host answers, so open views come back
    /// on their own whenever it does.
    Forever,
    /// `attempts` tries, each given `window` to connect and started a
    /// `window` after the one before, then [`Status::Unreachable`]. A person
    /// waiting on the screen learns within a bounded time that the host is
    /// out of reach instead of watching it connect indefinitely.
    Limited { attempts: u32, window: Duration },
}

/// What an agent view receives.
#[derive(Debug)]
pub enum AgentUpdate {
    /// The whole view, replacing everything before it: on the first attach
    /// and after every reconnect.
    Snapshot(Value),
    Ops(Value),
    /// The session is gone, or this device no longer reaches the host.
    Ended,
}

/// A view of an agent session on a host. Dropping it detaches.
pub struct AgentLink {
    host: Arc<RemoteHost>,
    session: String,
}

impl AgentLink {
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Run a command against the session and return its outcome.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.host
            .call(
                rpc::AGENT_CALL,
                &AgentCall {
                    session: self.session.clone(),
                    method: method.to_owned(),
                    params,
                },
            )
            .await
    }
}

impl Drop for AgentLink {
    fn drop(&mut self) {
        self.host.detach_agent(&self.session);
    }
}

/// What a terminal stream delivers to its [`NetworkPty`].
pub(crate) enum StreamEvent {
    Output(Vec<u8>),
    /// A full replay of the terminal that replaces everything before it.
    Checkpoint(Vec<u8>),
    /// The host PTY now has this size, set by another view.
    Size(u16, u16),
    Exit,
}

pub struct RemoteHost {
    id: DeviceId,
    key: Arc<DeviceKey>,
    app_version: String,
    retry: Retry,
    record: Mutex<PairedHost>,

    /// Called with the host record after each connection, which updates its
    /// address, name, and handshake time, so the caller can store it.
    on_record: Box<dyn Fn(PairedHost) + Send + Sync>,

    link: Mutex<Option<Link>>,
    views: Mutex<HashMap<String, View>>,
    agents: Mutex<HashMap<String, UnboundedSender<AgentUpdate>>>,
    status: watch::Sender<Status>,

    /// Bumped when the host reports its session list changed.
    sessions: watch::Sender<u64>,

    /// Views the host ended, and why. They stay open, showing that, and
    /// are not reattached after a reconnect until the user asks.
    ended: Mutex<HashMap<String, EndReason>>,

    /// Bumped when a view is ended or taken up again.
    ended_changes: watch::Sender<u64>,

    /// Wakes the supervisor: a view or request needs a link now.
    wake: Notify,

    /// Set while the application lists this host's sessions, which keeps
    /// the link up with no view open.
    listed: AtomicBool,

    next_id: AtomicU64,
    supervisor: Mutex<Option<AbortHandle>>,

    /// How the current link reaches the host, while one is up.
    path: Mutex<Option<LinkPath>>,

    /// Which paths links may take. A link on a path the policy no longer
    /// allows closes, and the next one follows the new policy.
    policy: watch::Sender<PathPolicy>,

    /// The STUN servers direct attempts ask; empty uses the built-in list.
    stun_servers: Mutex<Vec<String>>,
}

/// A LAN or direct channel opened while the link ran through the relay, with
/// the host record its handshake updated.
struct Upgrade {
    ws: BoxSocket,
    channel: Channel,
    path: LinkPath,
    record: PairedHost,
}

/// Why a link stopped being used.
enum LinkEnd {
    Closed,
    Moved(Box<Upgrade>),
}

/// A LAN or direct channel being opened in the background. Dropping it stops
/// the attempt, as when the link it would replace closes first.
struct Attempt(JoinHandle<Result<Upgrade>>);

impl Drop for Attempt {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Clone)]
struct Link {
    queue: SendQueue,
    calls: Arc<Mutex<HashMap<u64, PendingCall>>>,
    streams: Arc<Mutex<HashMap<u32, String>>>,
}

struct PendingCall {
    reply: oneshot::Sender<Result<Value, RpcError>>,

    /// For an attach: the view it is for. Handling the response before the
    /// next frame means what follows it (a terminal's checkpoint, an agent
    /// view's changes) never arrives ahead of it.
    attach: Option<Attach>,
}

enum Attach {
    Terminal(String),
    Agent(String),
}

struct View {
    events: UnboundedSender<StreamEvent>,
    stream: Option<u32>,

    /// Input typed before the view's first attach completes, sent once it
    /// does, so typing right after opening a tab loses nothing. After that,
    /// input during a reconnect is dropped instead of replayed later into a
    /// terminal whose state the user no longer sees.
    early_input: Option<Vec<u8>>,
}

/// Early input kept per view; a paste larger than this before the first
/// attach is cut.
const MAX_EARLY_INPUT: usize = 64 * 1024;

#[derive(Deserialize)]
struct AttachReply {
    stream: u32,
}

impl RemoteHost {
    pub fn new(
        record: PairedHost,
        key: Arc<DeviceKey>,
        app_version: String,
        retry: Retry,
        on_record: impl Fn(PairedHost) + Send + Sync + 'static,
    ) -> Arc<Self> {
        let host = Arc::new(Self {
            id: record.id.clone(),
            key,
            app_version,
            retry,
            record: Mutex::new(record),
            on_record: Box::new(on_record),
            link: Mutex::new(None),
            views: Mutex::new(HashMap::new()),
            agents: Mutex::new(HashMap::new()),
            status: watch::channel(Status::Idle).0,
            sessions: watch::channel(0).0,
            ended: Mutex::new(HashMap::new()),
            ended_changes: watch::channel(0).0,
            wake: Notify::new(),
            listed: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            supervisor: Mutex::new(None),
            path: Mutex::new(None),
            policy: watch::channel(PathPolicy::Auto).0,
            stun_servers: Mutex::new(Vec::new()),
        });

        let task = runtime().spawn(Arc::clone(&host).supervise());

        *host.supervisor.lock() = Some(task.abort_handle());

        host
    }

    pub fn id(&self) -> &DeviceId {
        &self.id
    }

    pub fn name(&self) -> String {
        self.record.lock().name.clone()
    }

    /// How the link reaches the host, while one is up.
    pub fn path(&self) -> Option<LinkPath> {
        self.path.lock().clone()
    }

    /// Restrict the paths links may take from now on. A link on a path the
    /// policy rules out closes and reconnects on an allowed one; a host
    /// shown as unreachable is tried again, since the new path may reach it.
    pub fn set_path_policy(&self, policy: PathPolicy) {
        if !self
            .policy
            .send_if_modified(|current| mem::replace(current, policy) != policy)
        {
            return;
        }

        if *self.status.borrow() == Status::Unreachable {
            self.wake.notify_one();
        }
    }

    /// Set the STUN servers later direct attempts ask, as `host:port`;
    /// empty uses the built-in list.
    pub fn set_stun_servers(&self, servers: Vec<String>) {
        *self.stun_servers.lock() = servers;
    }

    pub fn status(&self) -> watch::Receiver<Status> {
        self.status.subscribe()
    }

    /// Bumped whenever the host's session list changes.
    pub fn session_changes(&self) -> watch::Receiver<u64> {
        self.sessions.subscribe()
    }

    /// Why the host ended this device's view of `session`, while it stays
    /// ended.
    pub fn ended(&self, session: &str) -> Option<EndReason> {
        self.ended.lock().get(session).copied()
    }

    /// Bumped when a view is ended or taken up again.
    pub fn ended_changes(&self) -> watch::Receiver<u64> {
        self.ended_changes.subscribe()
    }

    /// Take up again a view the host ended, which takes the session back
    /// from the person at the host.
    pub fn reconnect(self: &Arc<Self>, session: &str) {
        if self.ended.lock().remove(session).is_none() {
            return;
        }

        self.ended_changes.send_modify(|version| *version += 1);

        let link = self.link.lock().clone();

        let Some(link) = link else {
            self.wake.notify_one();

            return;
        };

        if self.views.lock().contains_key(session) {
            self.attach(&link, session.to_owned());
        }

        if self.agents.lock().contains_key(session) {
            self.attach_agent(&link, session.to_owned());
        }
    }

    /// Record that the host ended `session`'s view here.
    fn end_view(&self, session: &str, reason: EndReason) {
        self.ended.lock().insert(session.to_owned(), reason);

        self.ended_changes.send_modify(|version| *version += 1);
    }

    /// Stop the connection for good, as when the host is forgotten.
    pub fn shutdown(&self) {
        if let Some(task) = self.supervisor.lock().take() {
            task.abort();
        }

        *self.link.lock() = None;

        self.end_views();
    }

    /// Tell every view the host is out of reach for good.
    fn end_views(&self) {
        for (_, view) in self.views.lock().drain() {
            let _ = view.events.send(StreamEvent::Exit);
        }

        for (_, agent) in self.agents.lock().drain() {
            let _ = agent.send(AgentUpdate::Ended);
        }
    }

    /// Whether anything needs the link: an open view, or a session list.
    fn has_views(&self) -> bool {
        self.listed.load(Ordering::Relaxed)
            || !self.views.lock().is_empty()
            || !self.agents.lock().is_empty()
    }

    /// Stay connected, reconnecting as needed, so the host's sessions can be
    /// listed without a view open.
    pub fn keep_connected(&self) {
        self.listed.store(true, Ordering::Relaxed);

        self.wake.notify_one();
    }

    /// Stop holding the link up for a session list. Open views keep it; with
    /// none, the link closes once it has been idle for a while.
    pub fn stop_listing(&self) {
        self.listed.store(false, Ordering::Relaxed);
    }

    /// A view of an agent session. It attaches now if connected and
    /// otherwise as soon as a link is up, and again after every reconnect,
    /// each time starting from a snapshot.
    pub fn agent_view(
        self: &Arc<Self>,
        session: String,
    ) -> (AgentLink, UnboundedReceiver<AgentUpdate>) {
        let (updates, updates_rx) = mpsc::unbounded_channel();

        self.agents.lock().insert(session.clone(), updates);

        if let Some(link) = self.link.lock().clone() {
            self.attach_agent(&link, session.clone());
        } else {
            self.wake.notify_one();
        }

        let link = AgentLink {
            host: Arc::clone(self),
            session,
        };

        (link, updates_rx)
    }

    fn detach_agent(&self, session: &str) {
        self.ended.lock().remove(session);

        if self.agents.lock().remove(session).is_some() {
            self.notify(
                rpc::AGENT_DETACH,
                &SessionRef {
                    session: session.to_owned(),
                },
            );
        }
    }

    fn attach_agent(self: &Arc<Self>, link: &Link, session: String) {
        let host = Arc::clone(self);
        let link = link.clone();

        runtime().spawn(async move {
            let (reply, response) = oneshot::channel();
            let id = host.next_id.fetch_add(1, Ordering::Relaxed);

            link.calls.lock().insert(
                id,
                PendingCall {
                    reply,
                    attach: Some(Attach::Agent(session.clone())),
                },
            );

            let request = SessionRef {
                session: session.clone(),
            };

            if send_request(&link, id, rpc::AGENT_ATTACH, &request).is_err() {
                return;
            }

            // A session that ended while this view was away is over; the
            // view stays open to say so.
            if let Ok(Err(error)) = response.await
                && error.code == ErrorCode::NotFound
                && host.agents.lock().contains_key(&session)
            {
                host.end_view(&session, EndReason::Closed);
            }
        });
    }

    /// Ask the host to push to this device while it is away.
    pub async fn register_push(&self, registration: &PushRegistration) -> Result<()> {
        let _: Value = self.call(PUSH_REGISTER, registration).await?;

        Ok(())
    }

    /// Ask the host to stop pushing to this device.
    pub async fn unregister_push(&self) -> Result<()> {
        let _: Value = self.call(PUSH_UNREGISTER, &Value::Null).await?;

        Ok(())
    }

    /// What this device may start on the host.
    pub async fn host_info(&self) -> Result<HostInfo> {
        self.call(rpc::HOST_INFO, &Value::Null).await
    }

    /// Start an agent tab on the host and return its session id.
    pub async fn open_agent(&self, profile: String, workspace: String) -> Result<String> {
        let SessionRef { session } = self
            .call(rpc::AGENT_OPEN, &AgentOpen { profile, workspace })
            .await?;

        Ok(session)
    }

    /// Start a terminal tab in a host workspace, on the named terminal
    /// profile or the host's default, and return its session id.
    pub async fn open_terminal_tab(
        &self,
        workspace: String,
        profile: Option<String>,
    ) -> Result<String> {
        let SessionRef { session } = self
            .call(
                rpc::TERMINAL_OPEN_TAB,
                &TerminalOpenTab { workspace, profile },
            )
            .await?;

        Ok(session)
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let list: SessionList = self.call(rpc::SESSIONS_LIST, &Value::Null).await?;

        Ok(list.sessions)
    }

    /// Start a terminal on the host and open a view of it.
    pub async fn open_terminal(self: &Arc<Self>, cols: u16, rows: u16) -> Result<NetworkPty> {
        let SessionRef { session } = self
            .call(rpc::TERMINAL_OPEN, &TerminalOpen { cols, rows })
            .await?;

        Ok(self.view(session))
    }

    /// A view of an existing session. It attaches now if connected and
    /// otherwise as soon as a link is up, and again after every reconnect.
    pub fn view(self: &Arc<Self>, session: String) -> NetworkPty {
        let (events, events_rx) = mpsc::unbounded_channel();

        self.views.lock().insert(
            session.clone(),
            View {
                events,
                stream: None,
                early_input: Some(Vec::new()),
            },
        );

        if let Some(link) = self.link.lock().clone() {
            self.attach(&link, session.clone());
        } else {
            self.wake.notify_one();
        }

        NetworkPty::new(Arc::clone(self), session, events_rx)
    }

    /// End a session on the host. Host tabs refuse: they belong to the
    /// person at the host.
    pub fn terminate(&self, session: &str) {
        self.notify(
            rpc::TERMINAL_CLOSE,
            &SessionRef {
                session: session.to_owned(),
            },
        );
    }

    /// End any session the host lists, host tabs included, and return once
    /// the host closed it. The error holds the host's reason for refusing.
    pub async fn close_session(&self, session: String) -> Result<()> {
        let _: Value = self
            .call(rpc::SESSION_CLOSE, &SessionRef { session })
            .await?;

        Ok(())
    }

    /// Rename a session the host lists, and return once the host renamed it.
    /// The error holds the host's reason for refusing.
    pub async fn rename_session(&self, session: String, title: String) -> Result<()> {
        let _: Value = self
            .call(rpc::SESSION_RENAME, &SessionRename { session, title })
            .await?;

        Ok(())
    }

    /// Give the host a new name, and return once the host took it. The
    /// host then sends it to every connected device, this one included, which
    /// updates the stored record. The error holds the host's reason for
    /// refusing.
    pub async fn rename_host(&self, name: String) -> Result<()> {
        let _: Value = self.call(rpc::HOST_RENAME, &HostName { name }).await?;

        Ok(())
    }

    /// Store a name the host announced while connected.
    fn renamed(&self, name: String) {
        let record = {
            let mut record = self.record.lock();

            if record.name == name {
                return;
            }

            record.name = name;

            record.clone()
        };

        (self.on_record)(record);
    }

    pub(crate) fn send_input(&self, session: &str, bytes: Vec<u8>) {
        let stream = {
            let mut views = self.views.lock();

            let Some(view) = views.get_mut(session) else {
                return;
            };

            match (view.stream, &mut view.early_input) {
                (Some(stream), _) => stream,
                (None, Some(early)) => {
                    let room = MAX_EARLY_INPUT.saturating_sub(early.len());

                    early.extend_from_slice(&bytes[..bytes.len().min(room)]);

                    return;
                }
                // Input typed while the link is down has no terminal to go
                // to; the tab shows that it is reconnecting.
                (None, None) => return,
            }
        };

        if let Some(link) = self.link.lock().as_ref() {
            link.queue.send(Outbound::new(stream, kind::INPUT, bytes));
        }
    }

    pub(crate) fn resize(&self, session: &str, cols: u16, rows: u16) {
        self.notify(
            rpc::TERMINAL_RESIZE,
            &TerminalResize {
                session: session.to_owned(),
                cols,
                rows,
            },
        );
    }

    /// The view closed; the session keeps running on the host.
    pub(crate) fn detach(&self, session: &str) {
        self.ended.lock().remove(session);

        let Some(view) = self.views.lock().remove(session) else {
            return;
        };

        if let Some(stream) = view.stream {
            if let Some(link) = self.link.lock().as_ref() {
                link.streams.lock().remove(&stream);
            }

            self.notify(rpc::STREAM_CLOSE, &StreamRef { stream });
        }
    }

    pub(crate) async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: &impl Serialize,
    ) -> Result<T> {
        let value = self
            .call_raw(method, params)
            .await?
            .map_err(|error| anyhow!("{method}: {}", error.message))?;

        Ok(serde_json::from_value(value)?)
    }

    async fn call_raw(
        &self,
        method: &str,
        params: &impl Serialize,
    ) -> Result<Result<Value, RpcError>> {
        let link = self.wait_for_link().await?;

        self.call_on(&link, method, params).await
    }

    /// Send a request on `link` and wait for its answer. A request that
    /// belongs to one channel, as signaling for a direct path does, fails
    /// with it instead of moving to the next link.
    async fn call_on(
        &self,
        link: &Link,
        method: &str,
        params: &impl Serialize,
    ) -> Result<Result<Value, RpcError>> {
        let (reply, response) = oneshot::channel();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        link.calls.lock().insert(
            id,
            PendingCall {
                reply,
                attach: None,
            },
        );

        send_request(link, id, method, params)?;

        response.await.map_err(|_| anyhow!("the connection closed"))
    }

    /// Send a request whose answer nobody waits for; the dispatcher drops
    /// answers to unknown ids. Nothing is sent without a link.
    fn notify(&self, method: &str, params: &impl Serialize) {
        if let Some(link) = self.link.lock().as_ref() {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);

            let _ = send_request(link, id, method, params);
        }
    }

    async fn wait_for_link(&self) -> Result<Link> {
        let mut status = self.status.subscribe();

        let deadline = Instant::now() + REQUEST_TIMEOUT;

        let mut woken = false;

        loop {
            if let Some(link) = self.link.lock().clone() {
                return Ok(link);
            }

            match *status.borrow_and_update() {
                Status::Refused => return Err(Refused.into()),
                // The round this request started has failed; starting
                // another would only hold the request until its deadline.
                Status::Unreachable if woken => {
                    return Err(anyhow!("{} is not reachable", self.name()));
                }
                // The supervisor waits for a wake in these states.
                Status::Idle | Status::Unreachable => self.wake.notify_one(),
                // Otherwise one wake cuts a backoff short. More would leave
                // a stored permit that starts another limited round as soon
                // as this one gives up.
                _ if !woken => self.wake.notify_one(),
                _ => {}
            }

            woken = true;

            if timeout(deadline - Instant::now(), status.changed())
                .await
                .is_err()
            {
                return Err(anyhow!("{} is not reachable", self.name()));
            }
        }
    }

    fn attach(self: &Arc<Self>, link: &Link, session: String) {
        let host = Arc::clone(self);
        let link = link.clone();

        // Views open from the UI thread, which has no runtime context.
        runtime().spawn(async move {
            let (reply, response) = oneshot::channel();
            let id = host.next_id.fetch_add(1, Ordering::Relaxed);

            link.calls.lock().insert(
                id,
                PendingCall {
                    reply,
                    attach: Some(Attach::Terminal(session.clone())),
                },
            );

            let request = SessionRef {
                session: session.clone(),
            };

            if send_request(&link, id, rpc::TERMINAL_ATTACH, &request).is_err() {
                return;
            }

            // A session that ended while this view was away is over; the
            // view stays open to say so.
            if let Ok(Err(error)) = response.await
                && error.code == ErrorCode::NotFound
                && host.views.lock().contains_key(&session)
            {
                host.end_view(&session, EndReason::Closed);
            }
        });
    }

    async fn supervise(self: Arc<Self>) {
        let mut backoff = MIN_BACKOFF;
        let mut failures = 0;
        let mut ever_connected = false;
        let mut network = netwatch::changes();

        // A LAN or direct channel the last link opened to move to, used
        // instead of connecting anew.
        let mut moved: Option<Box<Upgrade>> = None;

        loop {
            let (ws, channel, path) = match moved.take() {
                Some(upgrade) => {
                    let Upgrade {
                        ws,
                        channel,
                        path,
                        record,
                    } = *upgrade;

                    *self.record.lock() = record.clone();

                    (self.on_record)(record);

                    (ws, channel, path)
                }
                None => {
                    // A limited round runs to its end, so a request with no
                    // view open still gets every attempt.
                    if failures == 0 && !self.has_views() {
                        self.status.send_replace(Status::Idle);

                        self.wake.notified().await;
                    }

                    self.status.send_replace(if ever_connected {
                        Status::Reconnecting
                    } else {
                        Status::Connecting
                    });

                    let mut record = self.record.lock().clone();

                    let started = Instant::now();
                    let policy = *self.policy.borrow();

                    info!(host = %self.id, ?policy, "connecting to remote host");

                    let attempt = establish(&mut record, &self.key, &self.app_version, policy);

                    let result = match self.retry {
                        Retry::Forever => attempt.await,
                        Retry::Limited { window, .. } => timeout(window, attempt)
                            .await
                            .unwrap_or_else(|_| Err(anyhow!("connecting timed out"))),
                    };

                    match result {
                        Ok((ws, channel, path)) => {
                            *self.record.lock() = record.clone();

                            (self.on_record)(record);

                            backoff = MIN_BACKOFF;
                            failures = 0;
                            ever_connected = true;

                            (Box::new(ws) as BoxSocket, channel, path)
                        }
                        Err(error) if error.is::<Refused>() => {
                            info!(host = %self.id, "remote host refused authentication");

                            self.status.send_replace(Status::Refused);

                            self.end_views();

                            return;
                        }
                        Err(error) => {
                            info!(host = %self.id, elapsed_ms = started.elapsed().as_millis(),
                                error = %format_args!("{error:#}"), "connecting to the remote host failed");

                            if let Retry::Limited { attempts, window } = self.retry {
                                failures += 1;

                                if failures < attempts {
                                    sleep_until(started + window).await;

                                    continue;
                                }

                                failures = 0;

                                self.status.send_replace(Status::Unreachable);

                                // Wakes stored while the round ran came from
                                // requests and views that round already
                                // served; only a later one retries.
                                let _ = self.wake.notified().now_or_never();

                                network.mark_unchanged();

                                select! {
                                    () = self.wake.notified() => {}

                                    _ = network.changed() => {}
                                }

                                continue;
                            }

                            // A new view or request retries at once, and so
                            // does a network change, which may have brought
                            // the host back; otherwise the backoff spreads
                            // retries of many clients apart.
                            network.mark_unchanged();

                            select! {
                                () = sleep(jitter(backoff)) => {}

                                () = self.wake.notified() => {}

                                _ = network.changed() => {}
                            }

                            backoff = (backoff * 2).min(MAX_BACKOFF);

                            continue;
                        }
                    }
                }
            };

            let (queue, queue_rx) = SendQueue::new();
            let (inbound, inbound_rx) = mpsc::unbounded_channel();

            let link = Link {
                queue,
                calls: Arc::default(),
                streams: Arc::default(),
            };

            let probe = Arc::new(Notify::new());

            let pump = tokio::spawn(
                pump(ws, channel, queue_rx, inbound, Arc::clone(&probe))
                    .inspect_err(|error| {
                        info!(error = %format_args!("{error:#}"), "remote channel receive/send task failed");
                    })
                    .instrument(info_span!("remote_link", host = %self.id, ?path)),
            );

            *self.link.lock() = Some(link.clone());

            // Set before the status, so a watcher woken by `Connected`
            // reads the path of this link. Moving to the LAN sends
            // `Connected` again, which is how watchers learn the new path.
            *self.path.lock() = Some(path.clone());

            self.status.send_replace(Status::Connected);

            info!(host = %self.id, ?path, "connected to the remote host");

            // Views the host ended wait for the user to take them up.
            let ended = self.ended.lock().clone();

            let sessions: Vec<_> = self
                .views
                .lock()
                .keys()
                .filter(|session| !ended.contains_key(*session))
                .cloned()
                .collect();

            for session in sessions {
                self.attach(&link, session);
            }

            let agents: Vec<_> = self
                .agents
                .lock()
                .keys()
                .filter(|session| !ended.contains_key(*session))
                .cloned()
                .collect();

            for session in agents {
                self.attach_agent(&link, session);
            }

            network.mark_unchanged();

            let end = self
                .dispatch(&link, inbound_rx, &mut network, &probe, &path)
                .await;

            pump.abort();

            if let Err(error) = pump.await
                && !error.is_cancelled()
            {
                warn!(host = %self.id, ?path, %error, "remote channel task failed to complete");
            }

            *self.link.lock() = None;
            *self.path.lock() = None;

            link.calls.lock().clear();

            for view in self.views.lock().values_mut() {
                view.stream = None;
            }

            match end {
                LinkEnd::Closed => info!(host = %self.id, "the remote host link closed"),
                LinkEnd::Moved(upgrade) => {
                    info!(host = %self.id, path = ?upgrade.path, "moving the remote host link");

                    moved = Some(upgrade);
                }
            }
        }
    }

    /// Route one link's inbound messages until it closes or goes idle. A
    /// network change probes the link, which closes it if it no longer
    /// reaches the host.
    ///
    /// Under [`PathPolicy::Auto`], a link that is not on the LAN also tries
    /// the LAN now and then, a link through the relay tries a direct path,
    /// and a network change tries both at once. Once a channel is up on
    /// either and no request is waiting on this link, it ends with that
    /// channel to move to. A policy change that rules out the link's path
    /// closes it.
    async fn dispatch(
        self: &Arc<Self>,
        link: &Link,
        mut inbound: UnboundedReceiver<FrameMessage>,
        network: &mut watch::Receiver<u64>,
        probe: &Notify,
        path: &LinkPath,
    ) -> LinkEnd {
        let on_relay = *path == LinkPath::Relay;

        // The policy may have changed while this link was connecting, before
        // the subscription below could see it.
        let mut policy = self.policy.subscribe();

        let current = *policy.borrow_and_update();

        if !current.allows(path) {
            return LinkEnd::Closed;
        }

        let on_lan = matches!(path, LinkPath::Lan(_));

        let mut upgrading = !on_lan && current == PathPolicy::Auto;
        let mut going_direct = on_relay && current == PathPolicy::Auto;

        let mut idle_since: Option<Instant> = None;

        let mut next_attempt = Instant::now() + FIRST_LAN_ATTEMPT;
        let mut attempt: Option<Attempt> = None;
        let mut next_direct = Instant::now() + FIRST_DIRECT_ATTEMPT;
        let mut direct: Option<Attempt> = None;
        let mut ready: Option<(Box<Upgrade>, Instant)> = None;

        loop {
            if let Some((_, since)) = &ready {
                if link.calls.lock().is_empty() {
                    let (upgrade, _) = ready.take().expect("checked above");

                    return LinkEnd::Moved(upgrade);
                }

                if since.elapsed() >= UPGRADE_WAIT {
                    info!(host = %self.id, pending_requests = link.calls.lock().len(),
                        "discarding ready path: requests on current connection did not finish in time");

                    ready = None;
                    next_attempt = Instant::now() + LAN_RETRY;
                    next_direct = Instant::now() + DIRECT_RETRY;
                }
            }

            let due = upgrading && attempt.is_none() && ready.is_none();
            let direct_due = going_direct && direct.is_none() && ready.is_none();

            let message = select! {
                message = inbound.recv() => message,
                Ok(()) = policy.changed() => {
                    let current = *policy.borrow_and_update();

                    if !current.allows(path) {
                        info!(host = %self.id, ?current, "closing a link the path policy no longer allows");

                        return LinkEnd::Closed;
                    }

                    // Dropping a pending attempt stops it, and a channel
                    // already open closes unused.
                    upgrading = !on_lan && current == PathPolicy::Auto;
                    going_direct = on_relay && current == PathPolicy::Auto;

                    if !upgrading {
                        attempt = None;
                        direct = None;
                        ready = None;
                    }

                    continue;
                }

                Ok(()) = network.changed() => {
                    info!(host = %self.id, ?path, "network changed; probing current connection and retrying available paths");

                    probe.notify_one();

                    // The new network may be the host's, or its NAT may
                    // admit a direct path.
                    next_attempt = Instant::now();
                    next_direct = Instant::now();

                    continue;
                }

                () = sleep_until(next_attempt), if due => {
                    attempt = Some(self.try_lan());

                    continue;
                }

                () = sleep_until(next_direct), if direct_due => {
                    direct = Some(self.try_direct(link));

                    continue;
                }

                result = async { (&mut direct.as_mut().expect("guarded").0).await }, if direct.is_some() => {
                    direct = None;

                    match result {
                        Ok(Ok(upgrade)) if ready.is_none() => {
                            ready = Some((Box::new(upgrade), Instant::now()));
                        }
                        Ok(Ok(_)) => {}
                        Ok(Err(error)) => {
                            info!(host = %self.id, retry_secs = DIRECT_RETRY.as_secs(),
                                error = %format_args!("{error:#}"), "direct attempt failed; keeping relay connection");

                            next_direct = Instant::now() + DIRECT_RETRY;
                        }
                        Err(error) => {
                            warn!(host = %self.id, %error, "direct attempt task failed");
                            next_direct = Instant::now() + DIRECT_RETRY;
                        }
                    }

                    continue;
                }

                result = async { (&mut attempt.as_mut().expect("guarded").0).await }, if attempt.is_some() => {
                    attempt = None;

                    match result {
                        Ok(Ok(upgrade)) => ready = Some((Box::new(upgrade), Instant::now())),
                        Ok(Err(error)) => {
                            info!(host = %self.id, retry_secs = LAN_RETRY.as_secs(),
                                error = %format_args!("{error:#}"), "LAN attempt failed; keeping current connection");

                            next_attempt = Instant::now() + LAN_RETRY;
                        }
                        Err(error) => {
                            warn!(host = %self.id, %error, "LAN attempt task failed");
                            next_attempt = Instant::now() + LAN_RETRY;
                        }
                    }

                    continue;
                }

                () = sleep(Duration::from_secs(5)) => {
                    let idle = !self.has_views() && link.calls.lock().is_empty();

                    match (idle, idle_since) {
                        (false, _) => idle_since = None,
                        (true, None) => idle_since = Some(Instant::now()),
                        (true, Some(since)) if since.elapsed() >= IDLE_CLOSE => return LinkEnd::Closed,
                        (true, Some(_)) => {}
                    }

                    continue;
                }
            };

            let Some(message) = message else {
                return LinkEnd::Closed;
            };

            self.route(link, message);
        }
    }

    /// Open a LAN channel to the host in the background, from a copy of the
    /// host record that the handshake updates.
    fn try_lan(&self) -> Attempt {
        let mut record = self.record.lock().clone();

        let key = Arc::clone(&self.key);
        let app_version = self.app_version.clone();

        Attempt(runtime().spawn(async move {
            let (ws, channel, path) = establish_lan(&mut record, &key, &app_version).await?;

            Ok(Upgrade {
                ws: Box::new(ws),
                channel,
                path,
                record,
            })
        }))
    }

    /// Open a direct channel to the host in the background: gather STUN
    /// candidates, trade them with the host over `link`, then connect and
    /// run the channel handshake on the new path.
    fn try_direct(self: &Arc<Self>, link: &Link) -> Attempt {
        let host = Arc::clone(self);
        let link = link.clone();
        let servers = direct::stun_servers(&self.stun_servers.lock());
        let span = info_span!("direct_attempt", side = "client", host = %self.id);

        Attempt(
            runtime().spawn(
                async move {
                    let started = Instant::now();

                    let mut stage = "gather";

                    info!(
                        timeout_secs = DIRECT_TIMEOUT.as_secs(),
                        "starting direct attempt"
                    );

                    let attempt = async {
                        let gathered = direct::gather(&servers)
                            .await
                            .context("direct candidate gathering failed")?;

                        stage = "offer";
                        info!("sending direct offer");

                        let answer = host
                            .call_on(&link, rpc::DIRECT_OFFER, &gathered.offer())
                            .await
                            .context("direct offer exchange failed")?
                            .map_err(|error| anyhow!("{}: {}", rpc::DIRECT_OFFER, error.message))?;

                        let answer: DirectOffer = serde_json::from_value(answer)?;
                        let same_nat = gathered.same_nat(&answer);

                        info!(
                            peer_nat = ?answer.nat,
                            public_candidates = ?answer.addrs,
                            local_candidates = ?answer.local,
                            same_nat,
                            "received direct answer"
                        );

                        stage = "prepare";

                        let prepared = if same_nat {
                            direct::prepare_local(gathered, &answer, Side::Client)?
                        } else {
                            direct::prepare(gathered, &answer, Side::Client)?
                        };

                        stage = "connect";

                        let (ws, address) = prepared
                            .connect()
                            .await
                            .context("direct QUIC connection failed")?;

                        let mut record = host.record.lock().clone();

                        stage = "handshake";

                        let (ws, channel, path) = establish_direct(
                            &mut record,
                            &host.key,
                            &host.app_version,
                            ws,
                            address,
                        )
                        .await
                        .context("direct channel handshake failed")?;

                        info!(
                            elapsed_ms = started.elapsed().as_millis(),
                            ?path,
                            "direct channel ready for migration"
                        );

                        Result::<Upgrade>::Ok(Upgrade {
                            ws: Box::new(ws),
                            channel,
                            path,
                            record,
                        })
                    };

                    match timeout(DIRECT_TIMEOUT, attempt).await {
                        Ok(result) => result.with_context(|| {
                            format!(
                                "direct attempt failed during {stage} after {} ms",
                                started.elapsed().as_millis()
                            )
                        }),
                        Err(_) => Err(anyhow!(
                            "direct attempt timed out during {stage} after {} ms",
                            started.elapsed().as_millis()
                        )),
                    }
                }
                .instrument(span),
            ),
        )
    }

    fn route(&self, link: &Link, message: FrameMessage) {
        match (message.stream, message.kind) {
            (CONTROL_STREAM, kind::CONTROL_JSON) => match Control::decode(&message.payload) {
                Ok(Control::Response { id, outcome }) => {
                    let Some(call) = link.calls.lock().remove(&id) else {
                        return;
                    };

                    if let (Ok(result), Some(Attach::Agent(session))) = (&outcome, &call.attach)
                        && let Ok(AgentAttached { view }) =
                            serde_json::from_value::<AgentAttached>(result.clone())
                        && let Some(agent) = self.agents.lock().get(session)
                    {
                        let _ = agent.send(AgentUpdate::Snapshot(view));
                    }

                    if let (Ok(result), Some(Attach::Terminal(session))) = (&outcome, &call.attach)
                        && let Ok(AttachReply { stream }) =
                            serde_json::from_value::<AttachReply>(result.clone())
                        && let Some(view) = self.views.lock().get_mut(session)
                    {
                        view.stream = Some(stream);

                        link.streams.lock().insert(stream, session.clone());

                        if let Some(early) = view.early_input.take()
                            && !early.is_empty()
                        {
                            link.queue.send(Outbound::new(stream, kind::INPUT, early));
                        }
                    }

                    let _ = call.reply.send(outcome);
                }
                Ok(Control::Notification { method, .. }) if method == rpc::SESSIONS_CHANGED => {
                    self.sessions.send_modify(|version| *version += 1);
                }
                Ok(Control::Notification { method, params }) if method == rpc::HOST_RENAMED => {
                    if let Ok(HostName { name }) = serde_json::from_value(params) {
                        self.renamed(name);
                    }
                }
                Ok(Control::Notification { method, params }) if method == rpc::SESSION_ENDED => {
                    let Ok(SessionEnded { session, reason }) = serde_json::from_value(params)
                    else {
                        return;
                    };

                    // The host already dropped the stream; output for it is
                    // not coming, and input has nowhere to go.
                    if let Some(view) = self.views.lock().get_mut(&session)
                        && let Some(stream) = view.stream.take()
                    {
                        link.streams.lock().remove(&stream);
                    }

                    self.end_view(&session, reason);
                }
                Ok(Control::Notification { method, params }) if method == rpc::AGENT_OPS => {
                    if let Ok(AgentOps { session, ops }) = serde_json::from_value(params)
                        && let Some(agent) = self.agents.lock().get(&session)
                    {
                        let _ = agent.send(AgentUpdate::Ops(ops));
                    }
                }
                _ => {}
            },
            (stream, kind) => {
                let Some(session) = link.streams.lock().get(&stream).cloned() else {
                    return;
                };

                let event = match kind {
                    kind::OUTPUT => StreamEvent::Output(message.payload),
                    kind::CHECKPOINT => StreamEvent::Checkpoint(message.payload),
                    kind::SIZE => match serde_json::from_slice::<Size>(&message.payload) {
                        Ok(size) => StreamEvent::Size(size.cols, size.rows),
                        Err(_) => return,
                    },
                    kind::EXIT => {
                        link.streams.lock().remove(&stream);

                        if let Some(view) = self.views.lock().remove(&session) {
                            let _ = view.events.send(StreamEvent::Exit);
                        }

                        return;
                    }
                    // Unknown frame kinds come from a newer peer.
                    _ => return,
                };

                if let Some(view) = self.views.lock().get(&session) {
                    let _ = view.events.send(event);
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct Size {
    cols: u16,
    rows: u16,
}

fn send_request(link: &Link, id: u64, method: &str, params: &impl Serialize) -> Result<()> {
    let request = Control::Request {
        id,
        method: method.to_owned(),
        params: serde_json::to_value(params)?,
    };

    if link.queue.send(Outbound::new(
        CONTROL_STREAM,
        kind::CONTROL_JSON,
        request.encode(),
    )) {
        Ok(())
    } else {
        Err(anyhow!("the connection closed"))
    }
}

/// Up to a quarter more or less than `delay`, so clients that lost the same
/// host at the same moment do not all retry at the same instant.
fn jitter(delay: Duration) -> Duration {
    let mut byte = [0u8; 1];

    let _ = fill(&mut byte);

    let scale = 0.75 + f64::from(byte[0]) / 255.0 * 0.5;

    delay.mul_f64(scale)
}
