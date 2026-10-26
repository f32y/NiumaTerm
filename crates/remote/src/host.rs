//! The host side: accepts paired devices on the LAN and runs the terminals
//! they open.
//!
//! Every connection starts with the version preface and then either pairs
//! (only while a code is showing) or runs the IK channel handshake, which
//! answers only devices in the trust store. Terminals belong to the host,
//! not to a connection: a dropped connection leaves them running.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, TcpListener as StdTcpListener};
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use futures::{Sink, Stream};
use nmt_platform::runtime;
use nmt_remote_core::channel::{Channel, HostHandshake};
use nmt_remote_core::direct::{DirectOffer, Side};
use nmt_remote_core::frame::{CONTROL_STREAM, Message as FrameMessage, kind};
use nmt_remote_core::identity::{DeviceId, DeviceKey};
use nmt_remote_core::messages::{
    DeviceInfo, HostHello, MAX_DEVICE_NAME_CHARS, PairAccepted, RelayAccess, device_name,
};
#[cfg(test)]
use nmt_remote_core::pairing::CODE_LIFETIME_MS;
use nmt_remote_core::pairing::{HostPairing, IssuedCode, PairingCode};
use nmt_remote_core::preface::{Preface, PrefaceKind};
use nmt_remote_core::push::{
    PUSH_FEATURE, PUSH_REGISTER, PUSH_UNREGISTER, PushKind, PushMessage, PushRegistration, seal,
};
use nmt_remote_core::rpc::{
    self, AgentAttached, AgentCall, AgentOps, Attached, Control, ErrorCode, HostName, Origin,
    RpcError, SessionEnded, SessionList, SessionRef, SessionRename, StreamRef, TerminalOpen,
    TerminalResize,
};
use nmt_remote_core::{Error as CoreError, PROTO_MINOR};
use parking_lot::Mutex;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::{sleep, timeout};
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tracing::{Instrument as _, debug, info, info_span, warn};

use crate::direct;
use crate::discovery::Advertiser;
use crate::lan::lan_addresses;
use crate::link::{Outbound, SendQueue, pump, recv_binary, send_binary};
use crate::presence::{DISCONNECT_EXPIRY, DISCONNECT_GRACE, Presence, Presences};
use crate::push_sender::{self, Delivery};
use crate::relay::RelaySocket;
use crate::relay_host::{RelayCommand, run_host_link};
use crate::sessions::{AgentRequest, HostRequest, Kick, SessionRegistry, TerminalControl, Viewer};
use crate::store::{self, PairedDevice, now_ms};
use crate::stream::StreamFlow;

pub const DEFAULT_PORT: u16 = 47470;

/// Sockets that have not finished a handshake yet. Each costs a task and a
/// buffer, so an unauthenticated peer on the LAN cannot hold many.
const MAX_UNAUTHENTICATED: usize = 16;

/// How long one handshake step may take. A relay path crosses Cloudflare
/// twice per message, and a lossy route there spends seconds on TCP
/// retransmits, so this leaves room beyond a LAN round trip.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a whole pairing exchange may hold its socket: three round trips
/// at the client's per-message limit. It stays a total instead of a
/// per-message limit so a peer that trickles messages cannot keep one of the
/// few unauthenticated slots forever.
const PAIRING_TIMEOUT: Duration = Duration::from_secs(30);

const FEATURES: &[&str] = &["terminal", PUSH_FEATURE];

pub struct HostConfig {
    pub port: u16,
    pub device: DeviceInfo,

    /// The shell remote terminals start, normally the default profile's.
    /// `None` uses the platform default.
    pub shell: Option<String>,

    pub args: Vec<String>,

    /// The sessions offered to paired devices, host tabs included.
    pub registry: Arc<SessionRegistry>,

    /// The user's own relay, for devices off the LAN. Paired devices
    /// receive it, key included, inside the encrypted pairing exchange.
    pub relay: Option<RelayAccess>,

    /// Publish a DNS-SD record so LAN clients find this host without an
    /// address. Off keeps the computer name and device id off the network;
    /// devices then need an address or the relay.
    pub announce: bool,

    /// Runs on a runtime thread after pairing records change and when a
    /// device's presence changes, so a view that lists them can refresh.
    pub on_change: Arc<dyn Fn() + Send + Sync>,

    /// STUN servers for direct paths to devices on the relay, as
    /// `host:port`; empty uses the built-in list.
    pub stun_servers: Vec<String>,
}

/// A running host. Dropping it stops listening, closes every channel, and
/// ends the terminals remote devices opened.
pub struct HostService {
    shared: Arc<Shared>,
    local_addr: SocketAddr,
    task: JoinHandle<()>,

    /// Returns devices to paired when the person at the host uses it.
    desk: JoinHandle<()>,
}

struct Shared {
    key: DeviceKey,
    dir: PathBuf,
    config: HostConfig,

    /// The name devices see. It starts as `config.device.name` and changes
    /// without a restart, so connected devices keep their sessions.
    name: watch::Sender<String>,

    state: Mutex<State>,
    unauthenticated: Arc<Semaphore>,

    /// Absent while announcing is off or multicast is unavailable; clients
    /// then enter the address.
    advertiser: Mutex<Option<Advertiser>>,

    /// Commands to the relay link, when a relay is configured.
    relay: Option<UnboundedSender<RelayCommand>>,

    /// The pairing slot claimed on the relay, to release when the code goes.
    relay_slot: Mutex<Option<String>>,

    /// Posts pushes to the forwarders devices registered.
    push_client: reqwest::Client,

    /// The port the LAN listener took, which may differ from the configured
    /// one (0 picks any).
    lan_port: u16,

    /// Set while tests keep the LAN from answering.
    #[cfg(test)]
    lan_paused: AtomicBool,
}

#[derive(Default)]
struct State {
    devices: Vec<PairedDevice>,
    code: Option<IssuedCode>,

    /// Open channels by client key, so removing a device closes them.
    connections: Vec<([u8; 32], AbortHandle)>,

    presences: Presences,
}

/// A stream id with what attaching it needs: the session, its control
/// handle, and its PTY size watch.
type PendingAttach = (u32, String, TerminalControl, watch::Receiver<(u16, u16)>);

/// Per-channel request handling.
struct Connection {
    shared: Arc<Shared>,
    queue: SendQueue,

    /// The device on the other end, listed as a viewer of what it attaches.
    viewer: Viewer,

    /// Attached terminal streams and the session each shows.
    streams: HashMap<u32, (String, Arc<StreamFlow>)>,

    next_stream: u32,

    /// An attach whose response must be queued before its checkpoint.
    pending_attach: Option<PendingAttach>,

    /// The task forwarding each attached agent view's changes.
    agents: HashMap<String, AbortHandle>,

    /// Terminal attaches to host tabs the application has just started, by
    /// request id, to be handled again now that the session is live.
    started: UnboundedSender<(u64, Value)>,
}

impl HostService {
    /// Start listening. `dir` holds the trust store. The port falls back to an
    /// ephemeral one when the configured port is taken.
    pub fn start(dir: PathBuf, key: DeviceKey, config: HostConfig) -> Result<Self> {
        let listener = bind(config.port)?;

        listener.set_nonblocking(true)?;

        let listener = {
            let _runtime = runtime().enter();

            TcpListener::from_std(listener)?
        };

        let local_addr = listener.local_addr()?;

        let advertiser = if config.announce {
            start_advertiser(&config.device.name, &key, local_addr.port())
        } else {
            None
        };

        let relay_token = match &config.relay {
            Some(_) => store::load_or_create_relay_token(&dir)
                .inspect_err(|error| warn!(%error, "the relay is unavailable"))
                .ok(),
            None => None,
        };

        let (relay_commands, relay_rx) = mpsc::unbounded_channel();

        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                devices: store::load_devices(&dir),
                ..State::default()
            }),
            key,
            dir,
            name: watch::channel(config.device.name.clone()).0,
            config,
            unauthenticated: Arc::new(Semaphore::new(MAX_UNAUTHENTICATED)),
            advertiser: Mutex::new(advertiser),
            relay: relay_token.as_ref().map(|_| relay_commands),
            relay_slot: Mutex::new(None),
            push_client: push_sender::client(),
            lan_port: local_addr.port(),
            #[cfg(test)]
            lan_paused: Default::default(),
        });

        let task = runtime().spawn(accept_loop(Arc::clone(&shared), listener));
        let desk = runtime().spawn(watch_desk(Arc::clone(&shared)));

        if let (Some(relay), Some(token)) = (shared.config.relay.clone(), relay_token) {
            let served = Arc::downgrade(&shared);

            let serve: Arc<dyn Fn(RelaySocket) + Send + Sync> = Arc::new(move |ws| {
                let Some(shared) = served.upgrade() else {
                    return;
                };

                // Relay connections count against the same limit as LAN
                // sockets until their handshake completes.
                let Ok(permit) = Arc::clone(&shared.unauthenticated).try_acquire_owned() else {
                    info!(
                        path = "relay",
                        "rejecting remote connection: too many pending handshakes"
                    );

                    return;
                };

                let task = async move {
                    if let Err(error) = serve_ws(shared, ws, permit).await {
                        info!(error = %format_args!("{error:#}"), "relayed connection ended");
                    }
                };

                tokio::spawn(task.instrument(info_span!("remote_connection", path = "relay")));
            });

            runtime().spawn(run_host_link(
                relay,
                shared.key.id(),
                token,
                relay_rx,
                serve,
            ));
        }

        Ok(Self {
            shared,
            local_addr,
            task,
            desk,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn device_id(&self) -> DeviceId {
        self.shared.key.id()
    }

    /// Show a new pairing code, replacing any previous one.
    pub fn start_pairing(&self) -> Result<PairingCode> {
        let code = PairingCode::generate()?;

        self.shared.state.lock().code = Some(IssuedCode::new(code.clone(), now_ms()));

        self.shared.advertise_slot(Some(code.slot()));

        Ok(code)
    }

    /// Publish or withdraw this host's DNS-SD record without restarting
    /// it, so connected devices and the terminals they opened carry on.
    pub fn set_announced(&self, announced: bool) {
        let shared = &self.shared;

        let mut advertiser = shared.advertiser.lock();

        if !announced {
            *advertiser = None;

            return;
        }

        if advertiser.is_some() {
            return;
        }

        *advertiser = self.start_advertiser();
    }

    /// Go by `name` from now on without restarting, reporting whether it
    /// changed. Connected devices receive the new name at once, the DNS-SD
    /// record names it, and devices offline now read it from the handshake
    /// when they next connect.
    pub fn set_device_name(&self, name: String) -> bool {
        let changed = self.shared.name.send_if_modified(|current| {
            if *current == name {
                return false;
            }

            *current = name;

            true
        });

        if changed {
            let mut advertiser = self.shared.advertiser.lock();

            // An instance name is fixed once registered, so the record goes
            // and a new one takes its place.
            if advertiser.take().is_some() {
                *advertiser = self.start_advertiser();
            }
        }

        changed
    }

    fn start_advertiser(&self) -> Option<Advertiser> {
        let shared = &self.shared;
        let name = shared.name.borrow().clone();
        let started = start_advertiser(&name, &shared.key, shared.lan_port);

        // A code shown while the record was withdrawn still needs its slot,
        // or a client searching for it would never find this host.
        if let (Some(started), Some((code, _))) = (&started, self.pairing()) {
            started.set_pairing_slot(Some(code.slot()));
        }

        started
    }

    /// Replace a code that outlived its lifetime with a fresh one, reporting
    /// whether it did. A code that was used, cancelled, or refused after
    /// failed attempts is already cleared and stays cleared, so only the
    /// timeout keeps a pairing window going.
    pub fn renew_expired_pairing(&self) -> Result<bool> {
        let expired = |code: &Option<IssuedCode>| {
            code.as_ref()
                .is_some_and(|issued| !issued.is_usable(now_ms()))
        };

        if !expired(&self.shared.state.lock().code) {
            return Ok(false);
        }

        let code = PairingCode::generate()?;

        {
            let mut state = self.shared.state.lock();

            // A cancel may have arrived while the code was generated.
            if !expired(&state.code) {
                return Ok(false);
            }

            state.code = Some(IssuedCode::new(code.clone(), now_ms()));
        }

        self.shared.advertise_slot(Some(code.slot()));

        Ok(true)
    }

    pub fn cancel_pairing(&self) {
        self.shared.state.lock().code = None;

        self.shared.advertise_slot(None);
    }

    /// The code still accepting attempts, and when it expires.
    pub fn pairing(&self) -> Option<(PairingCode, u64)> {
        let state = self.shared.state.lock();
        let issued = state.code.as_ref()?;

        issued
            .is_usable(now_ms())
            .then(|| (issued.code().clone(), issued.expires_at_ms()))
    }

    /// Close LAN connections as they arrive, as a network that keeps devices
    /// apart would, until resumed; the relay keeps working.
    #[cfg(test)]
    pub(crate) fn pause_lan(&self, paused: bool) {
        self.shared.lan_paused.store(paused, Ordering::Relaxed);
    }

    /// Age the showing code past its lifetime, as five minutes would.
    #[cfg(test)]
    pub(crate) fn expire_pairing(&self) {
        let mut state = self.shared.state.lock();

        if let Some(code) = state.code.as_ref().map(|issued| issued.code().clone()) {
            state.code = Some(IssuedCode::new(code, now_ms() - CODE_LIFETIME_MS));
        }
    }

    /// Cut every open channel, as a network drop would.
    #[cfg(test)]
    pub(crate) fn drop_connections(&self) {
        for (_, task) in self.shared.state.lock().connections.drain(..) {
            task.abort();
        }
    }

    /// Stop accepting LAN connections, leaving the relay as the only way in.
    #[cfg(test)]
    pub(crate) fn close_lan(&self) {
        self.task.abort();
    }

    #[cfg(test)]
    pub(crate) fn terminal_count(&self) -> usize {
        self.shared.config.registry.remote_sessions().len()
    }

    pub fn devices(&self) -> Vec<PairedDevice> {
        self.shared.state.lock().devices.clone()
    }

    /// Names of the paired devices connected now, each device once. Only the
    /// connection lifecycle tests observe it; the app shows no live roster.
    #[cfg(test)]
    pub(crate) fn connected_devices(&self) -> Vec<String> {
        let state = self.shared.state.lock();

        state
            .devices
            .iter()
            .filter(|device| {
                state
                    .connections
                    .iter()
                    .any(|(key, _)| *key == device.public_key)
            })
            .map(|device| device.name.clone())
            .collect()
    }

    /// Whether the device is paired, connected, or connected before and
    /// gone now.
    pub fn presence(&self, id: &DeviceId) -> Presence {
        let state = self.shared.state.lock();

        state
            .devices
            .iter()
            .find(|device| &device.id == id)
            .map_or(Presence::Paired, |device| {
                state.presences.presence(&device.public_key, Instant::now())
            })
    }

    /// Tell every device that asked for `kind` about something that happened
    /// in `session`, whatever its presence. A LAN or direct channel of a
    /// suspended phone stays open until the liveness probes give up on it,
    /// so presence cannot tell whether the person is looking; the phone
    /// drops a push about the session it shows on screen instead.
    pub fn push(&self, session: &str, kind: PushKind, title: &str, body: &str) {
        let host = self.shared.key.id().as_str().to_owned();
        let message = PushMessage::new(&host, session, kind, title, body, now_ms());

        let targets: Vec<([u8; 32], PushRegistration)> = {
            let state = self.shared.state.lock();

            state
                .devices
                .iter()
                .filter_map(|device| {
                    let registration = device.push.as_ref()?;

                    registration
                        .kinds
                        .contains(&kind)
                        .then(|| (device.public_key, registration.clone()))
                })
                .collect()
        };

        // Who got a push, and who not, is otherwise invisible: a device that
        // never registered, or did not ask for this kind, is easy to mistake
        // for a failed delivery while trying pushes out.
        debug!(?kind, devices = targets.len(), "pushing to paired devices");

        for (device, registration) in targets {
            let sealed = match seal(&registration.key, &message) {
                Ok(sealed) => sealed,
                Err(error) => {
                    warn!(%error, "cannot seal a push");

                    continue;
                }
            };

            let shared = Arc::clone(&self.shared);
            let host = host.clone();
            let session = session.to_owned();

            runtime().spawn(async move {
                let delivery = push_sender::deliver(
                    &shared.push_client,
                    &registration,
                    &host,
                    &session,
                    &sealed,
                )
                .await;

                if delivery == Delivery::TokenGone {
                    shared.forget_push_token(&device, &registration.token);
                }
            });
        }
    }

    /// Revoke a device: forget its key and close its open channels.
    pub fn remove_device(&self, id: &DeviceId) -> Result<()> {
        let devices = {
            let mut state = self.shared.state.lock();

            let Some(key) = state
                .devices
                .iter()
                .find(|device| &device.id == id)
                .map(|device| device.public_key)
            else {
                return Ok(());
            };

            state.devices.retain(|device| device.public_key != key);
            state.presences.forget(&key);

            state.connections.retain(|(client, task)| {
                if *client == key {
                    task.abort();
                }

                *client != key
            });

            state.devices.clone()
        };

        store::save_devices(&self.shared.dir, &devices)?;

        Ok(())
    }
}

impl Drop for HostService {
    fn drop(&mut self) {
        self.task.abort();
        self.desk.abort();

        for (_, task) in self.shared.state.lock().connections.drain(..) {
            task.abort();
        }

        self.shared.config.registry.close_all_remote();
    }
}

impl Connection {
    fn handle(&mut self, message: FrameMessage) {
        match (message.stream, message.kind) {
            (CONTROL_STREAM, kind::CONTROL_JSON) => match Control::decode(&message.payload) {
                Ok(Control::Request { id, method, params })
                    if matches!(
                        method.as_str(),
                        rpc::AGENT_ATTACH
                            | rpc::AGENT_DETACH
                            | rpc::AGENT_CALL
                            | rpc::HOST_INFO
                            | rpc::AGENT_OPEN
                            | rpc::TERMINAL_OPEN_TAB
                            | rpc::SESSION_CLOSE
                            | rpc::SESSION_RENAME
                            | rpc::HOST_RENAME
                    ) =>
                {
                    self.agent_request(id, &method, params);
                }
                Ok(Control::Request { id, method, params }) if method == rpc::DIRECT_OFFER => {
                    self.direct_offer(id, params);
                }
                Ok(Control::Request { id, method, params }) => {
                    if method == rpc::TERMINAL_ATTACH
                        && let Ok(SessionRef { session }) = parse(params.clone())
                        && self.shared.config.registry.is_pending(&session)
                    {
                        self.start_then_attach(id, session, params);
                    } else {
                        self.answer(id, &method, params);
                    }
                }
                // Clients send only requests in this protocol revision.
                Ok(_) => {}
                Err(error) => debug!(%error, "ignoring an undecodable control message"),
            },
            (stream, kind::INPUT) => {
                if let Some((_, flow)) = self.streams.get(&stream) {
                    flow.input(message.payload);
                }
            }
            // Unknown frame kinds come from a newer peer and are skipped.
            _ => {}
        }
    }

    /// Answer a request handled in order with the others, then start the
    /// terminal stream an attach set up, which must follow its response.
    fn answer(&mut self, id: u64, method: &str, params: Value) {
        let outcome = self.request(method, params);

        self.queue.send(Outbound::new(
            CONTROL_STREAM,
            kind::CONTROL_JSON,
            Control::Response { id, outcome }.encode(),
        ));

        if let Some((stream, session, control, size)) = self.pending_attach.take() {
            let flow = StreamFlow::attach(stream, &control, size, self.queue.clone());

            self.shared
                .config
                .registry
                .add_viewer(&session, self.viewer.clone());

            self.streams.insert(stream, (session, flow));
        }
    }

    /// Attach to a host tab whose shell has not started yet. The tab lives
    /// on the UI thread, so the application starts it there, and the attach
    /// is handled again once the session is live under the same id.
    fn start_then_attach(&mut self, id: u64, session: String, params: Value) {
        let queue = self.queue.clone();
        let registry = Arc::clone(&self.shared.config.registry);
        let started = self.started.clone();

        let Some(host) = registry.host_requests() else {
            return respond(&queue, id, Err(RpcError::new(ErrorCode::NotFound, session)));
        };

        let (reply, answer) = oneshot::channel();

        if host
            .send(HostRequest::StartTab {
                session: session.clone(),
                reply,
            })
            .is_err()
        {
            return respond(&queue, id, Err(RpcError::new(ErrorCode::NotFound, session)));
        }

        tokio::spawn(async move {
            // A tab still listed as pending after starting would send the
            // attach around again forever, so it fails instead.
            let outcome = match answer.await {
                Ok(Ok(())) if !registry.is_pending(&session) => {
                    if started.send((id, params)).is_ok() {
                        return;
                    }

                    Err(RpcError::new(ErrorCode::Internal, "the channel closed"))
                }
                Ok(Ok(())) => Err(RpcError::new(ErrorCode::Internal, "the tab did not start")),
                Ok(Err(message)) => Err(RpcError::new(ErrorCode::NotFound, message)),
                Err(_) => Err(RpcError::new(ErrorCode::Internal, "the host stopped")),
            };

            respond(&queue, id, outcome);
        });
    }

    /// Answer a device's direct path offer with this host's candidates, then
    /// connect and serve the new path as one more connection. The reply
    /// waits until this side's endpoint is up, so a device that dials finds
    /// this side punching already.
    fn direct_offer(&self, id: u64, params: Value) {
        let offer: DirectOffer = match parse(params) {
            Ok(offer) => offer,
            Err(error) => {
                info!(request = id, "rejecting malformed direct offer");

                return respond(&self.queue, id, Err(error));
            }
        };

        // The direct connection counts against the same limit as LAN
        // sockets until its handshake completes.
        let Ok(permit) = Arc::clone(&self.shared.unauthenticated).try_acquire_owned() else {
            info!(
                request = id,
                "rejecting direct offer: too many pending handshakes"
            );

            return respond(
                &self.queue,
                id,
                Err(RpcError::new(
                    ErrorCode::Busy,
                    "too many pending handshakes",
                )),
            );
        };

        let shared = Arc::clone(&self.shared);
        let queue = self.queue.clone();

        let span = info_span!("direct_attempt", side = "host",
            device = %DeviceId::from_public_key(&self.viewer.key), request = id);

        let attempt = async move {
            let started = Instant::now();

            info!(peer_nat = ?offer.nat, public_candidates = ?offer.addrs,
                local_candidates = ?offer.local, "received direct offer");

            let servers = direct::stun_servers(&shared.config.stun_servers);

            let gathered = match direct::gather(&servers).await {
                Ok(gathered) => gathered,
                Err(error) => {
                    info!(stage = "gather", elapsed_ms = started.elapsed().as_millis(),
                        error = %format_args!("{error:#}"), "direct attempt failed; existing connection retained");

                    let error = RpcError::new(ErrorCode::Unsupported, error.to_string());

                    return respond(&queue, id, Err(error));
                }
            };

            let answer = gathered.offer();
            let same_nat = gathered.same_nat(&offer);

            info!(same_nat, "selecting route for direct offer");

            // The device applies the same checks to the same two offers, so
            // both pick one route and a pair that cannot meet stops on both
            // sides here.
            let prepared = if same_nat {
                direct::prepare_local(gathered, &offer, Side::Host)
            } else {
                direct::prepare(gathered, &offer, Side::Host)
            };

            respond(&queue, id, reply(&answer));

            info!("sent direct answer");

            let (stage, connected) = match prepared {
                Ok(prepared) => ("connect", prepared.connect().await),
                Err(error) => ("prepare", Err(error)),
            };

            let ws = match connected {
                Ok((ws, _)) => ws,
                Err(error) => {
                    info!(stage, elapsed_ms = started.elapsed().as_millis(),
                        error = %format_args!("{error:#}"), "direct attempt failed; existing connection retained");

                    return;
                }
            };

            info!(
                elapsed_ms = started.elapsed().as_millis(),
                "direct transport ready; starting channel handshake"
            );

            if let Err(error) = serve_ws(shared, ws, permit).await {
                info!(error = %format_args!("{error:#}"), "direct connection ended");
            }
        };

        tokio::spawn(attempt.instrument(span));
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        let registry = &self.shared.config.registry;

        match method {
            rpc::SESSIONS_LIST => reply(&SessionList {
                sessions: registry.list(),
            }),
            rpc::TERMINAL_OPEN => {
                let open: TerminalOpen = parse(params)?;

                let session = registry
                    .open_headless(
                        self.shared.config.shell.clone(),
                        self.shared.config.args.clone(),
                        open.cols.max(1),
                        open.rows.max(1),
                    )
                    .map_err(|error| RpcError::new(ErrorCode::Internal, error.to_string()))?;

                reply(&SessionRef { session })
            }
            rpc::TERMINAL_ATTACH => {
                let SessionRef { session } = parse(params)?;

                let (control, size) = registry
                    .attach_info(&session)
                    .ok_or_else(|| RpcError::new(ErrorCode::NotFound, &session))?;

                let (cols, rows) = *size.borrow();
                let stream = self.next_stream;

                self.next_stream += 1;
                self.pending_attach = Some((stream, session, control, size));

                reply(&Attached { stream, cols, rows })
            }
            rpc::TERMINAL_RESIZE => {
                let resize: TerminalResize = parse(params)?;

                if !registry.resize(&resize.session, resize.cols.max(1), resize.rows.max(1)) {
                    return Err(RpcError::new(ErrorCode::NotFound, resize.session));
                }

                Ok(Value::Null)
            }
            rpc::TERMINAL_CLOSE => {
                let SessionRef { session } = parse(params)?;

                match registry.origin(&session) {
                    None => Err(RpcError::new(ErrorCode::NotFound, session)),
                    Some(Origin::Remote) => {
                        registry.close_remote(&session);

                        Ok(Value::Null)
                    }
                    Some(_) => Err(RpcError::new(
                        ErrorCode::Denied,
                        "a host tab is closed on the host",
                    )),
                }
            }
            rpc::STREAM_CLOSE => {
                let StreamRef { stream } = parse(params)?;

                if let Some((session, flow)) = self.streams.remove(&stream) {
                    flow.close();

                    registry.remove_viewer(&session, &self.viewer.key);
                }

                Ok(Value::Null)
            }
            PUSH_REGISTER => {
                let registration: PushRegistration = parse(params)?;

                // A paired device can already run anything here; the check
                // only catches a malformed registration before a push fails
                // on it.
                if !registration.endpoint.starts_with("https://") {
                    return Err(RpcError::new(
                        ErrorCode::InvalidParams,
                        "the push endpoint must be an https URL",
                    ));
                }

                self.shared
                    .set_push(&self.viewer.key, Some(registration))
                    .map_err(|error| RpcError::new(ErrorCode::Internal, error.to_string()))?;

                Ok(done())
            }
            PUSH_UNREGISTER => {
                self.shared
                    .set_push(&self.viewer.key, None)
                    .map_err(|error| RpcError::new(ErrorCode::Internal, error.to_string()))?;

                Ok(done())
            }
            _ => Err(RpcError::new(ErrorCode::Unsupported, method)),
        }
    }
}

impl Connection {
    /// The host ended this device's views of a session: detach them here
    /// and tell the device why, so it shows that instead of an exit.
    fn kicked(&mut self, kick: Kick) {
        let streams: Vec<u32> = self
            .streams
            .iter()
            .filter(|(_, (session, _))| *session == kick.session)
            .map(|(stream, _)| *stream)
            .collect();

        for stream in streams {
            if let Some((_, flow)) = self.streams.remove(&stream) {
                flow.close();
            }
        }

        if let Some(task) = self.agents.remove(&kick.session) {
            task.abort();
        }

        let notice = Control::Notification {
            method: rpc::SESSION_ENDED.into(),
            params: serde_json::json!(SessionEnded {
                session: kick.session,
                reason: kick.reason,
            }),
        };

        self.queue.send(Outbound::new(
            CONTROL_STREAM,
            kind::CONTROL_JSON,
            notice.encode(),
        ));
    }
}

impl Connection {
    /// Agent sessions and the application answer on the UI thread, so
    /// their replies go out from tasks once they arrive, not in order with
    /// other requests.
    fn agent_request(&mut self, id: u64, method: &str, params: Value) {
        let queue = self.queue.clone();
        let registry = &self.shared.config.registry;

        let control = |session: &str| {
            registry
                .agent(session)
                .ok_or_else(|| RpcError::new(ErrorCode::NotFound, session))
        };

        match method {
            rpc::HOST_INFO | rpc::AGENT_OPEN | rpc::TERMINAL_OPEN_TAB => {
                let Some(host) = registry.host_requests() else {
                    return respond(
                        &queue,
                        id,
                        Err(RpcError::new(ErrorCode::Unsupported, method)),
                    );
                };

                let (reply, answer) = oneshot::channel();

                let request = match method {
                    rpc::HOST_INFO => HostRequest::Info { reply },
                    rpc::AGENT_OPEN => HostRequest::OpenAgent { params, reply },
                    _ => HostRequest::OpenTerminalTab { params, reply },
                };

                if host.send(request).is_err() {
                    return respond(
                        &queue,
                        id,
                        Err(RpcError::new(ErrorCode::Unsupported, method)),
                    );
                }

                tokio::spawn(async move {
                    // A refused open names a profile or workspace the host
                    // does not offer, which is the request's fault.
                    let outcome = match answer.await {
                        Ok(Ok(value)) => Ok(value),
                        Ok(Err(message)) => Err(RpcError::new(ErrorCode::InvalidParams, message)),
                        Err(_) => Err(RpcError::new(ErrorCode::Internal, "the host stopped")),
                    };

                    respond(&queue, id, outcome);
                });
            }
            rpc::SESSION_CLOSE => {
                let session = match parse::<SessionRef>(params) {
                    Ok(SessionRef { session }) => session,
                    Err(error) => return respond(&queue, id, Err(error)),
                };

                match registry.origin(&session) {
                    None => respond(&queue, id, Err(RpcError::new(ErrorCode::NotFound, session))),
                    // Nothing on the host shows a headless terminal on its
                    // own, so it ends here without asking the application.
                    Some(Origin::Remote) => {
                        registry.close_remote(&session);

                        respond(&queue, id, Ok(done()));
                    }
                    Some(_) => {
                        let Some(host) = registry.host_requests() else {
                            return respond(
                                &queue,
                                id,
                                Err(RpcError::new(ErrorCode::Unsupported, method)),
                            );
                        };

                        let (reply, answer) = oneshot::channel();

                        if host
                            .send(HostRequest::CloseSession { session, reply })
                            .is_err()
                        {
                            return respond(
                                &queue,
                                id,
                                Err(RpcError::new(ErrorCode::Unsupported, method)),
                            );
                        }

                        tokio::spawn(async move {
                            let outcome = match answer.await {
                                Ok(Ok(())) => Ok(done()),
                                Ok(Err(message)) => Err(RpcError::new(ErrorCode::Denied, message)),
                                Err(_) => {
                                    Err(RpcError::new(ErrorCode::Internal, "the host stopped"))
                                }
                            };

                            respond(&queue, id, outcome);
                        });
                    }
                }
            }
            rpc::HOST_RENAME => {
                let name = match parse::<HostName>(params) {
                    Ok(HostName { name }) => name,
                    Err(error) => return respond(&queue, id, Err(error)),
                };

                let Some(name) = device_name(&name) else {
                    return respond(
                        &queue,
                        id,
                        Err(RpcError::new(
                            ErrorCode::InvalidParams,
                            format!(
                                "a name must not be empty or longer than {MAX_DEVICE_NAME_CHARS} \
                                 characters"
                            ),
                        )),
                    );
                };

                let Some(host) = registry.host_requests() else {
                    return respond(
                        &queue,
                        id,
                        Err(RpcError::new(ErrorCode::Unsupported, method)),
                    );
                };

                let (reply, answer) = oneshot::channel();

                if host.send(HostRequest::RenameHost { name, reply }).is_err() {
                    return respond(
                        &queue,
                        id,
                        Err(RpcError::new(ErrorCode::Unsupported, method)),
                    );
                }

                tokio::spawn(async move {
                    let outcome = match answer.await {
                        Ok(Ok(())) => Ok(done()),
                        Ok(Err(message)) => Err(RpcError::new(ErrorCode::Denied, message)),
                        Err(_) => Err(RpcError::new(ErrorCode::Internal, "the host stopped")),
                    };

                    respond(&queue, id, outcome);
                });
            }
            rpc::SESSION_RENAME => {
                let SessionRename { session, title } = match parse(params) {
                    Ok(rename) => rename,
                    Err(error) => return respond(&queue, id, Err(error)),
                };

                let title = title.trim().to_owned();

                if title.is_empty() {
                    return respond(
                        &queue,
                        id,
                        Err(RpcError::new(ErrorCode::InvalidParams, "the name is empty")),
                    );
                }

                match registry.origin(&session) {
                    None => respond(&queue, id, Err(RpcError::new(ErrorCode::NotFound, session))),
                    // No tab on the host shows a headless terminal, so its
                    // listed name is the only one it has.
                    Some(Origin::Remote) => {
                        registry.set_title(&session, title);

                        respond(&queue, id, Ok(done()));
                    }
                    Some(_) => {
                        let Some(host) = registry.host_requests() else {
                            return respond(
                                &queue,
                                id,
                                Err(RpcError::new(ErrorCode::Unsupported, method)),
                            );
                        };

                        let (reply, answer) = oneshot::channel();

                        if host
                            .send(HostRequest::RenameSession {
                                session,
                                title,
                                reply,
                            })
                            .is_err()
                        {
                            return respond(
                                &queue,
                                id,
                                Err(RpcError::new(ErrorCode::Unsupported, method)),
                            );
                        }

                        tokio::spawn(async move {
                            let outcome = match answer.await {
                                Ok(Ok(())) => Ok(done()),
                                Ok(Err(message)) => {
                                    Err(RpcError::new(ErrorCode::NotFound, message))
                                }
                                Err(_) => {
                                    Err(RpcError::new(ErrorCode::Internal, "the host stopped"))
                                }
                            };

                            respond(&queue, id, outcome);
                        });
                    }
                }
            }
            rpc::AGENT_ATTACH => {
                let attach = parse::<SessionRef>(params).and_then(|SessionRef { session }| {
                    control(&session).map(|control| (session, control))
                });

                let (session, control) = match attach {
                    Ok(attach) => attach,
                    Err(error) => return respond(&queue, id, Err(error)),
                };

                let (updates, mut updates_rx) = mpsc::unbounded_channel();

                let (snapshot, view) = oneshot::channel();

                if control
                    .requests
                    .send(AgentRequest::Attach {
                        updates,
                        reply: snapshot,
                    })
                    .is_err()
                {
                    return respond(&queue, id, Err(RpcError::new(ErrorCode::NotFound, session)));
                }

                let forwarded = session.clone();

                // One task answers and then forwards, so no change can reach
                // the client ahead of the snapshot it applies to.
                let task = tokio::spawn(async move {
                    let outcome = match view.await {
                        Ok(view) => reply(&AgentAttached { view }),
                        Err(_) => Err(RpcError::new(ErrorCode::NotFound, &forwarded)),
                    };

                    let attached = outcome.is_ok();

                    respond(&queue, id, outcome);

                    if !attached {
                        return;
                    }

                    while let Some(ops) = updates_rx.recv().await {
                        let notice = Control::Notification {
                            method: rpc::AGENT_OPS.into(),
                            params: serde_json::json!(AgentOps {
                                session: forwarded.clone(),
                                ops,
                            }),
                        };

                        if !queue.send(Outbound::new(
                            CONTROL_STREAM,
                            kind::CONTROL_JSON,
                            notice.encode(),
                        )) {
                            break;
                        }
                    }
                });

                // A repeated attach replaces the view, so the device is
                // still one viewer.
                match self.agents.insert(session.clone(), task.abort_handle()) {
                    Some(previous) => previous.abort(),
                    None => registry.add_viewer(&session, self.viewer.clone()),
                }
            }
            rpc::AGENT_DETACH => {
                let outcome = parse::<SessionRef>(params).map(|SessionRef { session }| {
                    if let Some(task) = self.agents.remove(&session) {
                        task.abort();

                        registry.remove_viewer(&session, &self.viewer.key);
                    }

                    Value::Null
                });

                respond(&queue, id, outcome);
            }
            _ => {
                let call = parse::<AgentCall>(params)
                    .and_then(|call| control(&call.session).map(|control| (call, control)));

                let (call, control) = match call {
                    Ok(call) => call,
                    Err(error) => return respond(&queue, id, Err(error)),
                };

                // Only a device viewing the session drives it. One the host
                // took the session back from lost its view, and with it the
                // right to send commands until it attaches again.
                if !self.agents.contains_key(&call.session) {
                    return respond(
                        &queue,
                        id,
                        Err(RpcError::new(
                            ErrorCode::Denied,
                            "this device does not have the session",
                        )),
                    );
                }

                let (answer, outcome) = oneshot::channel();

                let sent = control.requests.send(AgentRequest::Call {
                    method: call.method,
                    params: call.params,
                    reply: answer,
                });

                if sent.is_err() {
                    return respond(
                        &queue,
                        id,
                        Err(RpcError::new(ErrorCode::NotFound, call.session)),
                    );
                }

                tokio::spawn(async move {
                    let outcome = match outcome.await {
                        Ok(Ok(value)) => Ok(value),
                        Ok(Err(message)) => Err(RpcError::new(ErrorCode::Internal, message)),
                        Err(_) => Err(RpcError::new(ErrorCode::NotFound, call.session)),
                    };

                    respond(&queue, id, outcome);
                });
            }
        }
    }
}

impl Shared {
    /// Where devices on this machine's LAN reach the host now. Sent at
    /// pairing and with every handshake, so a device that came in through
    /// the relay can go direct once it is on the same network, and learns
    /// the host's new address after DHCP moved it.
    fn lan_hints(&self) -> Vec<String> {
        lan_addresses()
            .into_iter()
            .map(|ip| format!("{ip}:{}", self.lan_port))
            .collect()
    }

    /// Record where the device with `key` wants pushes, or that it wants
    /// none.
    fn set_push(&self, key: &[u8; 32], push: Option<PushRegistration>) -> Result<()> {
        let devices = {
            let mut state = self.state.lock();

            let Some(device) = state
                .devices
                .iter_mut()
                .find(|device| &device.public_key == key)
            else {
                return Ok(());
            };

            device.push = push;

            state.devices.clone()
        };

        store::save_devices(&self.dir, &devices)?;

        Ok(())
    }

    /// Stop pushing to a token APNs refused, unless the device registered a
    /// new one meanwhile.
    fn forget_push_token(&self, key: &[u8; 32], token: &str) {
        let current = self
            .state
            .lock()
            .devices
            .iter()
            .find(|device| &device.public_key == key)
            .and_then(|device| device.push.as_ref())
            .is_some_and(|push| push.token == token);

        if current && let Err(error) = self.set_push(key, None) {
            warn!(%error, "cannot drop a push token APNs refused");
        }
    }

    /// Publish the showing code's slot on the LAN and the relay, or
    /// withdraw it.
    fn advertise_slot(&self, slot: Option<&str>) {
        if let Some(advertiser) = &*self.advertiser.lock() {
            advertiser.set_pairing_slot(slot);
        }

        if let Some(relay) = &self.relay {
            let mut claimed = self.relay_slot.lock();

            if let Some(previous) = claimed.take() {
                let _ = relay.send(RelayCommand::ReleaseSlot(previous));
            }

            if let Some(slot) = slot {
                *claimed = Some(slot.to_owned());

                let _ = relay.send(RelayCommand::ClaimSlot(slot.to_owned()));
            }
        }
    }
}

fn start_advertiser(name: &str, key: &DeviceKey, port: u16) -> Option<Advertiser> {
    Advertiser::start(name, key.id(), port)
        .inspect_err(|error| warn!(%error, "LAN discovery is unavailable"))
        .ok()
}

fn bind(port: u16) -> Result<StdTcpListener> {
    // IPv4 covers the home and office LANs this path targets; an IPv6
    // listener joins it once DNS-SD discovery advertises both families.
    match StdTcpListener::bind((Ipv4Addr::UNSPECIFIED, port)) {
        Err(error) if error.kind() == ErrorKind::AddrInUse && port != 0 => {
            warn!(port, "remote port is taken; listening on an ephemeral port");

            Ok(StdTcpListener::bind((Ipv4Addr::UNSPECIFIED, 0))?)
        }
        result => Ok(result?),
    }
}

async fn accept_loop(shared: Arc<Shared>, listener: TcpListener) {
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                warn!(%error, "remote listener accept failed");

                continue;
            }
        };

        #[cfg(test)]
        if shared.lan_paused.load(Ordering::Relaxed) {
            continue;
        }

        let Ok(permit) = Arc::clone(&shared.unauthenticated).try_acquire_owned() else {
            info!(%peer, "dropping a connection: too many pending handshakes");

            continue;
        };

        let shared = Arc::clone(&shared);

        tokio::spawn(
            async move {
                if let Err(error) = serve(shared, tcp, permit).await {
                    info!(error = %format_args!("{error:#}"), "remote connection ended");
                }
            }
            .instrument(info_span!("remote_connection", path = "lan", %peer)),
        );
    }
}

async fn serve(shared: Arc<Shared>, tcp: TcpStream, permit: OwnedSemaphorePermit) -> Result<()> {
    let ws = timeout(HANDSHAKE_TIMEOUT, accept_hdr_async(tcp, check_path))
        .await
        .context("LAN WebSocket handshake timed out")?
        .context("LAN WebSocket handshake failed")?;

    serve_ws(shared, ws, permit).await
}

/// Run one connection, from the LAN or through the relay: the version
/// preface, then pairing or the channel.
async fn serve_ws<S>(shared: Arc<Shared>, mut ws: S, permit: OwnedSemaphorePermit) -> Result<()>
where
    S: Stream<Item = Result<Message, WsError>>
        + Sink<Message, Error = WsError>
        + Unpin
        + Send
        + 'static,
{
    info!("starting remote protocol handshake");

    let (offer, answer) = timeout(HANDSHAKE_TIMEOUT, async {
        let offer = Preface::decode(&recv_binary(&mut ws).await?)?;
        let answer = offer.answer();

        send_binary(&mut ws, answer.encode().to_vec()).await?;

        anyhow::Ok((offer, answer))
    })
    .await
    .context("remote protocol preface timed out")?
    .context("remote protocol preface failed")?;

    match answer.kind {
        PrefaceKind::Rejected => {
            info!("remote protocol preface rejected");

            Ok(())
        }
        PrefaceKind::Pairing => timeout(
            PAIRING_TIMEOUT,
            serve_pairing(&shared, &mut ws, &offer, &answer),
        )
        .await
        .context("remote pairing timed out")?
        .context("remote pairing failed"),
        PrefaceKind::Channel => {
            let (channel, client_key) = timeout(
                HANDSHAKE_TIMEOUT,
                accept_channel(&shared, &mut ws, &offer, &answer),
            )
            .await
            .context("remote channel handshake timed out")?
            .context("remote channel handshake failed")?;

            drop(permit);

            run_channel(shared, ws, channel, client_key)
                .instrument(
                    info_span!("remote_channel", device = %DeviceId::from_public_key(&client_key)),
                )
                .await
        }
    }
}

// tungstenite fixes this callback's signature, error type included.
#[allow(clippy::result_large_err)]
fn check_path(request: &Request, response: Response) -> Result<Response, ErrorResponse> {
    if request.uri().path() == "/v1" {
        return Ok(response);
    }

    let mut rejection = ErrorResponse::new(None);

    *rejection.status_mut() = StatusCode::NOT_FOUND;

    Err(rejection)
}

async fn serve_pairing<S>(
    shared: &Shared,
    ws: &mut S,
    offer: &Preface,
    answer: &Preface,
) -> Result<()>
where
    S: Stream<Item = Result<Message, WsError>> + Sink<Message, Error = WsError> + Unpin,
{
    let hello = recv_binary(ws).await?;

    let code = {
        let state = shared.state.lock();

        match &state.code {
            Some(issued) if issued.is_usable(now_ms()) => issued.code().clone(),
            _ => bail!("no pairing code is showing"),
        }
    };

    let (mut pairing, reply) = HostPairing::on_hello(&code, &shared.key, offer, answer, &hello)?;

    send_binary(ws, reply).await?;

    let msg1 = recv_binary(ws).await?;

    send_binary(ws, pairing.on_msg1(&msg1)?).await?;

    let msg3 = recv_binary(ws).await?;

    let request = match pairing.on_msg3(&msg3) {
        Err(CoreError::WrongCode) => {
            {
                let mut state = shared.state.lock();

                if let Some(issued) = state.code.as_mut()
                    && issued.code() == &code
                    && !issued.record_failure(now_ms())
                {
                    state.code = None;

                    drop(state);

                    shared.advertise_slot(None);
                }
            }

            (shared.config.on_change)();

            bail!("wrong pairing code");
        }
        result => result?,
    };

    let devices = {
        let mut state = shared.state.lock();

        // Another attempt may have used or replaced the code meanwhile.
        match &state.code {
            Some(issued) if issued.code() == &code && issued.is_usable(now_ms()) => {}
            _ => bail!("the pairing code is no longer valid"),
        }

        state.code = None;

        state
            .devices
            .retain(|device| device.public_key != request.client_key);

        state.devices.push(PairedDevice::new(
            request.client_key,
            request.client.name.clone(),
            request.client.kind,
            request.client.platform.clone(),
        ));

        state.devices.clone()
    };

    shared.advertise_slot(None);

    store::save_devices(&shared.dir, &devices)?;

    let accepted = request.accept(&PairAccepted {
        host: DeviceInfo {
            name: shared.name.borrow().clone(),
            ..shared.config.device.clone()
        },
        relay: shared.config.relay.clone(),
        lan_hints: shared.lan_hints(),
    })?;

    send_binary(ws, accepted).await?;

    (shared.config.on_change)();

    Ok(())
}

async fn accept_channel<S>(
    shared: &Shared,
    ws: &mut S,
    offer: &Preface,
    answer: &Preface,
) -> Result<(Channel, [u8; 32])>
where
    S: Stream<Item = Result<Message, WsError>> + Sink<Message, Error = WsError> + Unpin,
{
    let msg1 = recv_binary(ws).await?;
    let pending = HostHandshake::read(&shared.key, offer, answer, &msg1)?;
    let client_key = *pending.client_key();
    let hello_ms = pending.hello().hello_ms;

    let devices = {
        let mut state = shared.state.lock();

        // An unknown or revoked key gets no reply at all.
        let Some(device) = state
            .devices
            .iter_mut()
            .find(|device| device.public_key == client_key)
        else {
            bail!("unpaired device");
        };

        if hello_ms <= device.last_hello_ms {
            bail!("replayed handshake from {}", device.id);
        }

        device.last_hello_ms = hello_ms;
        device.last_seen = now_ms();

        state.devices.clone()
    };

    if let Err(error) = store::save_devices(&shared.dir, &devices) {
        warn!(%error, "failed to record a device connection");
    }

    // Both sides speak the lower revision. The lint sees today's value of
    // 0; the rule holds for every later one.
    #[allow(clippy::unnecessary_min_or_max)]
    let proto_minor = PROTO_MINOR.min(pending.hello().proto_minor);

    let hello = HostHello {
        proto_minor,
        app_version: shared.config.device.app_version.clone(),
        features: FEATURES.iter().map(|&feature| feature.into()).collect(),
        name: shared.name.borrow().clone(),
        lan_hints: shared.lan_hints(),
    };

    let (channel, msg2) = pending.accept(&hello)?;

    send_binary(ws, msg2).await?;

    Ok((channel, client_key))
}

async fn run_channel<S>(
    shared: Arc<Shared>,
    ws: S,
    channel: Channel,
    client_key: [u8; 32],
) -> Result<()>
where
    S: Stream<Item = Result<Message, WsError>>
        + Sink<Message, Error = WsError>
        + Unpin
        + Send
        + 'static,
{
    let (queue, queue_rx) = SendQueue::new();

    let (in_tx, mut in_rx) = mpsc::unbounded_channel();

    // The host answers probes and waits out quiet links; a device whose
    // network changed runs the fast check from its side.
    let pump = tokio::spawn(pump(ws, channel, queue_rx, in_tx, Arc::default()));
    let pump_id = pump.id();
    let channel_started = Instant::now();

    info!(connection = %pump_id, "remote channel authenticated");

    // Clients that listed sessions refresh when the list changes, and
    // every client receives a new host name.
    let mut changes = shared.config.registry.subscribe();
    let mut names = shared.name.subscribe();

    let notices = queue.clone();

    let watcher = tokio::spawn(async move {
        loop {
            let notice = tokio::select! {
                changed = changes.changed() => match changed {
                    Ok(()) => Control::Notification {
                        method: rpc::SESSIONS_CHANGED.into(),
                        params: Value::Null,
                    },
                    Err(_) => break,
                },
                changed = names.changed() => match changed {
                    Ok(()) => Control::Notification {
                        method: rpc::HOST_RENAMED.into(),
                        params: serde_json::json!(HostName {
                            name: names.borrow_and_update().clone(),
                        }),
                    },
                    Err(_) => break,
                },
            };

            if !notices.send(Outbound::new(
                CONTROL_STREAM,
                kind::CONTROL_JSON,
                notice.encode(),
            )) {
                break;
            }
        }
    });

    let name = {
        let mut state = shared.state.lock();

        state.connections.push((client_key, pump.abort_handle()));
        state.presences.opened(client_key);

        state
            .devices
            .iter()
            .find(|device| device.public_key == client_key)
            .map(|device| device.name.clone())
            .unwrap_or_default()
    };

    (shared.config.on_change)();

    let (kicks, mut kicks_rx) = mpsc::unbounded_channel();
    let (started, mut started_rx) = mpsc::unbounded_channel();

    let mut connection = Connection {
        shared: Arc::clone(&shared),
        queue,
        viewer: Viewer {
            key: client_key,
            name,
            kicks,
        },
        streams: HashMap::new(),
        next_stream: 1,
        pending_attach: None,
        agents: HashMap::new(),
        started,
    };

    loop {
        tokio::select! {
            message = in_rx.recv() => match message {
                Some(message) => connection.handle(message),
                None => break,
            },
            Some(kick) = kicks_rx.recv() => connection.kicked(kick),
            Some((id, params)) = started_rx.recv() => {
                connection.answer(id, rpc::TERMINAL_ATTACH, params);
            }
        }
    }

    {
        let mut state = shared.state.lock();

        state.connections.retain(|(_, task)| task.id() != pump_id);
        state.presences.closed(client_key, Instant::now());
    }

    (shared.config.on_change)();

    // Presence moves on by itself once the grace and the expiry pass; a view
    // listing devices learns of it then. A stale refresh after a reconnect
    // only redraws the same state.
    for delay in [DISCONNECT_GRACE, DISCONNECT_EXPIRY] {
        let on_change = Arc::clone(&shared.config.on_change);

        tokio::spawn(async move {
            sleep(delay).await;

            on_change();
        });
    }

    watcher.abort();

    let registry = &shared.config.registry;

    // The views on this channel detach; their sessions keep running.
    for (session, flow) in connection.streams.values() {
        flow.close();

        registry.remove_viewer(session, &client_key);
    }

    for (session, task) in &connection.agents {
        task.abort();

        registry.remove_viewer(session, &client_key);
    }

    let result = match pump.await {
        Ok(result) => result,
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(error.into()),
    };

    if result.is_ok() {
        info!(connection = %pump_id, elapsed_ms = channel_started.elapsed().as_millis(), "remote channel closed");
    }

    result.with_context(|| {
        format!(
            "remote channel {pump_id} for device {} failed after {} ms",
            DeviceId::from_public_key(&client_key),
            channel_started.elapsed().as_millis()
        )
    })
}

/// The person at the host using it means devices away from it are no longer
/// following it, so they stop counting as disconnected.
async fn watch_desk(shared: Arc<Shared>) {
    let mut uses = shared.config.registry.subscribe_local_use();

    while uses.changed().await.is_ok() {
        shared.state.lock().presences.at_desk();

        (shared.config.on_change)();
    }
}

fn respond(queue: &SendQueue, id: u64, outcome: Result<Value, RpcError>) {
    queue.send(Outbound::new(
        CONTROL_STREAM,
        kind::CONTROL_JSON,
        Control::Response { id, outcome }.encode(),
    ));
}

/// The result of a request that has nothing to report. A `null` result reads
/// as a missing one to every released client, which then drops the response
/// and waits forever, so success is an empty object.
fn done() -> Value {
    Value::Object(Default::default())
}

fn parse<T: DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    serde_json::from_value(params)
        .map_err(|error| RpcError::new(ErrorCode::InvalidParams, error.to_string()))
}

fn reply<T: Serialize>(value: &T) -> Result<Value, RpcError> {
    serde_json::to_value(value)
        .map_err(|error| RpcError::new(ErrorCode::Internal, error.to_string()))
}
