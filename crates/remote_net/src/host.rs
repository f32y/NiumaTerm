use std::collections::HashMap;
use std::fmt::Display;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};
use std::{io, thread};

use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use nmt_remote_protocol::{
    ClientBound, Frame, Handshake, HostBound, MAX_DATA_LEN, PairingCode, SecureChannel,
    StaticKeypair, WireSessionInfo, WireSessionOptions, WireSessionSnapshot, derive_host_id,
    new_pairing_token,
};
use nmt_remote_session_hub::{
    RemoteSessionHub, SessionEvent, SessionId, SessionInfo, SessionOptions,
};
use parking_lot::Mutex;
use tokio::runtime::Builder as RuntimeBuilder;
use tokio::sync::{mpsc, watch};
use tokio::time;
use tokio_tungstenite::tungstenite::protocol::Message;
use tracing::{info, warn};

use crate::{
    AuthorizedDevices, CONNECT_MODE_IK, CONNECT_MODE_PAIR, KeyStoreError, NetError,
    RelayControlMessage, WsStream, hex_encode, load_or_create_keypair, next_binary, relay_ws_url,
    ws_connect,
};

const PAIRING_TTL: Duration = Duration::from_secs(300);
const CONTROL_PING_INTERVAL: Duration = Duration::from_secs(20);
const RECONNECT_CAP: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct HostConfig {
    /// Relay endpoint, e.g. `wss://relay.example.com/ws`.
    pub relay_url: String,
    pub access_token: String,
    /// Directory for `host-key.json` and `authorized_devices.json`.
    pub data_dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum HostStartError {
    #[error(transparent)]
    Key(#[from] KeyStoreError),
    #[error("device list unavailable: {0}")]
    Devices(io::Error),
    #[error("tokio runtime: {0}")]
    Runtime(io::Error),
}

struct PendingPairing {
    token: [u8; 16],
    expires: Instant,
}

struct ActiveConnection {
    device_public_key: Vec<u8>,
    cancel: watch::Sender<bool>,
}

struct Shared {
    config: HostConfig,
    keys: StaticKeypair,
    host_id: String,
    hub: RemoteSessionHub,
    devices: Mutex<AuthorizedDevices>,
    pending_pairing: Mutex<Option<PendingPairing>>,
    active: Mutex<HashMap<String, ActiveConnection>>,
    shutdown: AtomicBool,
}

/// Handle to the running host service. The service outlives dropped handles
/// only until `shutdown()`; sessions themselves live in the hub and survive
/// client disconnects by design.
pub struct HostHandle {
    shared: Arc<Shared>,
}

impl HostHandle {
    /// Load identity + device list and start the relay-facing service on its
    /// own tokio runtime thread, keeping the GPUI main thread untouched.
    pub fn start(config: HostConfig) -> Result<HostHandle, HostStartError> {
        let keys = load_or_create_keypair(&config.data_dir.join("host-key.json"))?;
        let devices = AuthorizedDevices::load(config.data_dir.join("authorized_devices.json"))
            .map_err(HostStartError::Devices)?;
        let host_id = derive_host_id(&keys.public);
        info!(host_id, "starting remote session host");

        let shared = Arc::new(Shared {
            config,
            keys,
            host_id,
            hub: RemoteSessionHub::new(),
            devices: Mutex::new(devices),
            pending_pairing: Mutex::new(None),
            active: Mutex::new(HashMap::new()),
            shutdown: AtomicBool::new(false),
        });

        let runtime = RuntimeBuilder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(HostStartError::Runtime)?;
        let control = Arc::clone(&shared);
        thread::Builder::new()
            .name("remote-host".into())
            .spawn(move || runtime.block_on(control_loop(control)))
            .map_err(HostStartError::Runtime)?;

        Ok(HostHandle { shared })
    }

    pub fn host_id(&self) -> &str {
        &self.shared.host_id
    }

    pub fn public_key(&self) -> &[u8] {
        &self.shared.keys.public
    }

    /// Issue a fresh one-time pairing code (TTL 5 minutes). Replaces any
    /// outstanding code — only the newest one can be redeemed.
    pub fn begin_pairing(&self) -> PairingCode {
        let token = new_pairing_token();
        *self.shared.pending_pairing.lock() = Some(PendingPairing {
            token,
            expires: Instant::now() + PAIRING_TTL,
        });
        PairingCode {
            relay_url: self.shared.config.relay_url.clone(),
            host_id: self.shared.host_id.clone(),
            host_public_key: self
                .shared
                .keys
                .public
                .as_slice()
                .try_into()
                .expect("X25519 public key is 32 bytes"),
            token,
        }
    }

    pub fn list_devices(&self) -> Vec<crate::DeviceEntry> {
        self.shared.devices.lock().entries().to_vec()
    }

    /// Revoke a device: future IK handshakes fail and live connections from
    /// that device are dropped immediately.
    pub fn revoke_device(&self, public_key_hex: &str) -> io::Result<bool> {
        let removed = self.shared.devices.lock().remove(public_key_hex)?;
        if removed {
            let active = self.shared.active.lock();
            for conn in active.values() {
                if hex_encode(&conn.device_public_key) == public_key_hex {
                    let _ = conn.cancel.send(true);
                }
            }
        }
        Ok(removed)
    }

    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        let active = self.shared.active.lock();
        for conn in active.values() {
            let _ = conn.cancel.send(true);
        }
    }
}

async fn control_loop(shared: Arc<Shared>) {
    let mut attempt: u32 = 0;
    loop {
        if shared.shutdown.load(Ordering::SeqCst) {
            return;
        }
        let url = relay_ws_url(&shared.config.relay_url, &shared.host_id, "host", None);
        match ws_connect(&url, Some(&shared.config.access_token)).await {
            Ok(ws) => {
                attempt = 0;
                info!("relay control socket registered");
                run_control(&shared, ws).await;
                warn!("relay control socket lost");
            }
            Err(e) => warn!("relay control connect failed: {e}"),
        }
        if shared.shutdown.load(Ordering::SeqCst) {
            return;
        }
        attempt = attempt.saturating_add(1);
        let backoff = RECONNECT_CAP.min(Duration::from_millis(1000) * attempt);
        time::sleep(backoff).await;
    }
}

async fn run_control(shared: &Arc<Shared>, mut ws: WsStream) {
    let mut ping = time::interval(CONTROL_PING_INTERVAL);
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if ws.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return;
                }
            }
            msg = ws.next() => {
                let text = match msg {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Close(_))) | None => return,
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => { warn!("control socket error: {e}"); return }
                };
                let Ok(control) = serde_json::from_str::<RelayControlMessage>(&text) else {
                    warn!("unparseable relay control message: {text}");
                    continue;
                };
                match control {
                    RelayControlMessage::Connected { connection_id } => {
                        spawn_connection(shared, connection_id);
                    }
                    RelayControlMessage::Sync { connections } => {
                        // Reconciliation after (re)registering: open data
                        // sockets for clients we don't serve yet, drop ones
                        // the relay no longer knows.
                        let active = shared.active.lock();
                        for (cid, conn) in active.iter() {
                            if !connections.contains(cid) {
                                let _ = conn.cancel.send(true);
                            }
                        }
                        let missing: Vec<String> = connections
                            .into_iter()
                            .filter(|cid| !active.contains_key(cid))
                            .collect();
                        drop(active);
                        for cid in missing {
                            spawn_connection(shared, cid);
                        }
                    }
                    RelayControlMessage::Disconnected { connection_id } => {
                        if let Some(conn) =
                            shared.active.lock().remove(&connection_id)
                        {
                            let _ = conn.cancel.send(true);
                        }
                    }
                }
            }
        }
    }
}

fn spawn_connection(shared: &Arc<Shared>, cid: String) {
    let shared = Arc::clone(shared);
    tokio::spawn(async move {
        if let Err(e) = serve_connection(&shared, &cid).await {
            info!("connection {cid} ended: {e}");
        }
        shared.active.lock().remove(&cid);
    });
}

async fn serve_connection(shared: &Arc<Shared>, cid: &str) -> Result<(), NetError> {
    let url = relay_ws_url(&shared.config.relay_url, &shared.host_id, "host", Some(cid));
    let mut ws = ws_connect(&url, Some(&shared.config.access_token)).await?;

    // First client message: mode prefix + Noise message 1.
    let first = next_binary(&mut ws).await?;
    let (&mode, msg1) = first
        .split_first()
        .ok_or_else(|| NetError::Protocol("empty first message".into()))?;

    let (chan, device_public_key) = match mode {
        CONNECT_MODE_IK => {
            let mut handshake = Handshake::responder_ik(&shared.keys.private)?;
            handshake.read_message(msg1)?;
            let remote = handshake
                .remote_static()
                .ok_or_else(|| NetError::Protocol("IK peer sent no static key".into()))?
                .to_vec();
            // The authorization gate: unknown device keys never get a reply,
            // so no session data (not even a handshake completion) leaks.
            if !shared.devices.lock().contains(&remote) {
                let _ = ws.close(None).await;
                return Err(NetError::Protocol("unauthorized device".into()));
            }
            let msg2 = handshake.write_message()?;
            ws.send(Message::Binary(msg2.into())).await?;
            (handshake.into_transport()?, remote)
        }
        CONNECT_MODE_PAIR => {
            let mut handshake = Handshake::responder_xx(&shared.keys.private)?;
            handshake.read_message(msg1)?;
            let msg2 = handshake.write_message()?;
            ws.send(Message::Binary(msg2.into())).await?;
            handshake.read_message(&next_binary(&mut ws).await?)?;
            let remote = handshake
                .remote_static()
                .ok_or_else(|| NetError::Protocol("XX peer sent no static key".into()))?
                .to_vec();
            let mut chan = handshake.into_transport()?;

            // The channel is encrypted but the peer is untrusted until the
            // one-time token is redeemed; the only acceptable first frame is
            // Pair.
            let ciphertext = next_binary(&mut ws).await?;
            let frame = Frame::decode(&chan.open(&ciphertext)?)?;
            let Frame::Control(payload) = frame else {
                return Err(NetError::Protocol("expected Pair control frame".into()));
            };
            let HostBound::Pair { token, device_name } = Frame::parse_control(&payload)? else {
                return Err(NetError::Protocol("expected Pair control frame".into()));
            };
            if !redeem_pairing_token(shared, token) {
                send_frame(
                    &mut ws,
                    &mut chan,
                    &Frame::control(&ClientBound::Error {
                        session_id: None,
                        message: "pairing token invalid or expired".into(),
                    })?,
                )
                .await?;
                let _ = ws.close(None).await;
                return Err(NetError::Protocol("bad pairing token".into()));
            }
            shared
                .devices
                .lock()
                .add(&device_name, &remote)
                .map_err(|e| NetError::Protocol(format!("persisting device failed: {e}")))?;
            info!(device = %device_name, "device paired");
            send_frame(&mut ws, &mut chan, &Frame::control(&ClientBound::Paired)?).await?;
            (chan, remote)
        }
        other => {
            return Err(NetError::Protocol(format!(
                "unknown connect mode {other:#04x}"
            )));
        }
    };

    let (cancel_tx, cancel_rx) = watch::channel(false);
    shared.active.lock().insert(
        cid.to_owned(),
        ActiveConnection {
            device_public_key,
            cancel: cancel_tx,
        },
    );
    serve_session(shared, ws, chan, cancel_rx).await
}

async fn send_frame(
    ws: &mut WsStream,
    chan: &mut SecureChannel,
    frame: &Frame,
) -> Result<(), NetError> {
    let ciphertext = chan.seal(&frame.encode()?)?;
    ws.send(Message::Binary(ciphertext.into())).await?;
    Ok(())
}

/// Bridge from the hub's blocking std receiver to the async serving loop.
/// One std thread per attached session: it parks in `recv_timeout` and exits
/// on cancel, on hub-side detach (overflow), or when the serving loop dies.
struct SubscriptionBridge {
    cancel: Arc<AtomicBool>,
}

impl SubscriptionBridge {
    fn spawn(
        subscription: nmt_remote_session_hub::SessionSubscription,
        session_id: u64,
        events: mpsc::UnboundedSender<(u64, SessionEvent)>,
    ) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match subscription
                    .events()
                    .recv_timeout(Duration::from_millis(500))
                {
                    Ok(event) => {
                        if events.send((session_id, event)).is_err() {
                            break;
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            // `subscription` drops here, unregistering the subscriber while
            // leaving the shell running (detach semantics).
        });
        Self { cancel }
    }
}

impl Drop for SubscriptionBridge {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

async fn serve_session(
    shared: &Arc<Shared>,
    ws: WsStream,
    mut chan: SecureChannel,
    mut cancel: watch::Receiver<bool>,
) -> Result<(), NetError> {
    let (mut sink, mut stream) = ws.split();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<(u64, SessionEvent)>();
    let mut bridges: HashMap<u64, SubscriptionBridge> = HashMap::new();

    loop {
        tokio::select! {
            biased;
            changed = cancel.changed() => {
                // A dropped sender (connection already deregistered) cancels
                // just like an explicit `send(true)`.
                if changed.is_err() || *cancel.borrow() {
                    let _ = sink.close().await;
                    return Ok(());
                }
            }
            event = event_rx.recv() => {
                // Sender lives in this scope, so the channel can't close.
                let Some((session_id, event)) = event else { return Ok(()) };
                match event {
                    SessionEvent::Output { seq, data } => {
                        // Chunk to respect the Noise message cap; chunks keep
                        // the event's seq — ordering is what clients rely on.
                        for piece in data.chunks(MAX_DATA_LEN) {
                            let frame = Frame::Output {
                                session_id,
                                seq,
                                data: piece.to_vec(),
                            };
                            send_split(&mut sink, &mut chan, &frame).await?;
                        }
                    }
                    SessionEvent::Exited { seq } => {
                        bridges.remove(&session_id);
                        send_split(&mut sink, &mut chan, &Frame::Exited { session_id, seq })
                            .await?;
                    }
                }
            }
            msg = stream.next() => {
                let data = match msg {
                    Some(Ok(Message::Binary(data))) => data,
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => return Err(e.into()),
                };
                let frame = Frame::decode(&chan.open(&data)?)?;
                if let Some(reply) =
                    handle_frame(shared, frame, &event_tx, &mut bridges)
                {
                    send_split(&mut sink, &mut chan, &reply?).await?;
                }
            }
        }
    }
}

async fn send_split(
    sink: &mut SplitSink<WsStream, Message>,
    chan: &mut SecureChannel,
    frame: &Frame,
) -> Result<(), NetError> {
    let ciphertext = chan.seal(&frame.encode()?)?;
    sink.send(Message::Binary(ciphertext.into())).await?;
    Ok(())
}

/// Dispatch one inbound frame against the hub. Returns the reply to send, if
/// any; hub errors become Error frames instead of killing the connection.
fn handle_frame(
    shared: &Arc<Shared>,
    frame: Frame,
    event_tx: &mpsc::UnboundedSender<(u64, SessionEvent)>,
    bridges: &mut HashMap<u64, SubscriptionBridge>,
) -> Option<Result<Frame, NetError>> {
    let reply = |msg: &ClientBound| Some(Frame::control(msg).map_err(NetError::from));
    let error = |session_id: Option<u64>, e: &dyn Display| {
        reply(&ClientBound::Error {
            session_id,
            message: e.to_string(),
        })
    };
    match frame {
        Frame::Control(payload) => {
            let msg = match Frame::parse_control::<HostBound>(&payload) {
                Ok(msg) => msg,
                Err(e) => return Some(Err(e.into())),
            };
            match msg {
                HostBound::ListSessions => {
                    let sessions = shared
                        .hub
                        .list_sessions()
                        .into_iter()
                        .map(to_wire_info)
                        .collect();
                    reply(&ClientBound::SessionList(sessions))
                }
                HostBound::Open(options) => match shared.hub.open(to_hub_options(options)) {
                    Ok(id) => reply(&ClientBound::Opened { session_id: id.0 }),
                    Err(e) => error(None, &e),
                },
                HostBound::Attach { session_id } => {
                    match shared.hub.attach(SessionId(session_id)) {
                        Ok(subscription) => {
                            let snapshot = to_wire_snapshot(subscription.snapshot());
                            bridges.insert(
                                session_id,
                                SubscriptionBridge::spawn(
                                    subscription,
                                    session_id,
                                    event_tx.clone(),
                                ),
                            );
                            reply(&ClientBound::Attached(snapshot))
                        }
                        Err(e) => error(Some(session_id), &e),
                    }
                }
                HostBound::Detach { session_id } => {
                    bridges.remove(&session_id);
                    None
                }
                HostBound::Kill { session_id } => match shared.hub.kill(SessionId(session_id)) {
                    Ok(()) => None,
                    Err(e) => error(Some(session_id), &e),
                },
                HostBound::Pair { .. } => error(
                    None,
                    &"pairing is only accepted as the first frame of a pairing connection",
                ),
            }
        }
        Frame::Input { session_id, data } => {
            match shared.hub.write_input(SessionId(session_id), &data) {
                Ok(()) => None,
                Err(e) => error(Some(session_id), &e),
            }
        }
        Frame::Resize {
            session_id,
            cols,
            rows,
        } => match shared.hub.resize(SessionId(session_id), cols, rows) {
            Ok(()) => None,
            Err(e) => error(Some(session_id), &e),
        },
        // Output/Exited only flow host → client.
        Frame::Output { session_id, .. } | Frame::Exited { session_id, .. } => {
            error(Some(session_id), &"unexpected server-bound data frame")
        }
    }
}

fn redeem_pairing_token(shared: &Arc<Shared>, token: [u8; 16]) -> bool {
    let mut pending = shared.pending_pairing.lock();
    match pending.take() {
        // One-shot by construction: `take()` consumes the pending entry, so a
        // second redeem — even with the right token — fails until the user
        // issues a new code.
        Some(p) if p.token == token && Instant::now() < p.expires => true,
        _ => false,
    }
}

fn to_hub_options(wire: WireSessionOptions) -> SessionOptions {
    let mut options = SessionOptions::default();
    if let Some(shell) = wire.shell {
        options.shell = shell;
    }
    options.working_directory = wire.working_directory;
    if wire.cols > 0 {
        options.cols = wire.cols;
    }
    if wire.rows > 0 {
        options.rows = wire.rows;
    }
    options
}

fn to_wire_info(info: SessionInfo) -> WireSessionInfo {
    WireSessionInfo {
        session_id: info.id.0,
        shell: info.shell,
        title: info.title.unwrap_or_default(),
        exited: info.exited,
        attached_clients: info.attached_clients as u32,
    }
}

fn to_wire_snapshot(snapshot: &nmt_remote_session_hub::SessionSnapshot) -> WireSessionSnapshot {
    WireSessionSnapshot {
        session_id: snapshot.session_id.0,
        base_seq: snapshot.base_seq,
        vt: snapshot.vt.clone(),
        cols: snapshot.cols,
        rows: snapshot.rows,
    }
}
