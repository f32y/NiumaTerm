use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::runtime::Builder as RuntimeBuilder;
use tokio::sync::mpsc;
use tokio::time;
use tokio_tungstenite::tungstenite::protocol::Message;
use tracing::{info, warn};

use crate::protocol::{
    ClientBound, Frame, HostBound, PairingCode, ProtocolSessionInfo, ProtocolSessionOptions,
    ProtocolSessionSnapshot, StaticKeypair,
};
use crate::{FrameChannel, NET_TIMEOUT, NetError, client_connect_ik, with_timeout};

/// One remote session's byte stream as the terminal engine wants to consume
/// it: opaque output bytes plus a terminal exit. Mirrors the hub's
/// `SessionEvent` but flattened to what a PTY reader yields.
#[derive(Debug)]
pub enum SessionByteEvent {
    Output(Vec<u8>),
    Exited,
}

/// Client-side handle to one attached remote session. Its receiver/senders are
/// std (not tokio) types so the synchronous terminal PTY adapter (`NetPty`) can
/// use them directly without touching the async runtime.
pub struct RemoteSession {
    pub session_id: u64,
    pub snapshot: ProtocolSessionSnapshot,
    output: std_mpsc::Receiver<SessionByteEvent>,
    commands: mpsc::UnboundedSender<Frame>,
}

impl RemoteSession {
    pub fn output(&self) -> &std_mpsc::Receiver<SessionByteEvent> {
        &self.output
    }

    pub fn send_input(&self, data: Vec<u8>) {
        let _ = self.commands.send(Frame::Input {
            session_id: self.session_id,
            data,
        });
    }

    pub fn send_resize(&self, cols: u16, rows: u16) {
        let _ = self.commands.send(Frame::Resize {
            session_id: self.session_id,
            cols,
            rows,
        });
    }

    pub fn snapshot(&self) -> &ProtocolSessionSnapshot {
        &self.snapshot
    }

    /// A cloneable input handle usable independently of the output receiver, so
    /// the terminal PTY adapter can own the byte stream while the UI still sends
    /// input/resize.
    pub fn input(&self) -> RemoteInput {
        RemoteInput {
            session_id: self.session_id,
            commands: self.commands.clone(),
        }
    }

    /// Consume the session into its output receiver (the byte stream a PTY
    /// reader drains). Pair with a prior `input()` for the write side.
    pub fn into_output(self) -> std_mpsc::Receiver<SessionByteEvent> {
        self.output
    }
}

/// Write side of a remote session: input and resize, decoupled from the output
/// receiver so both halves can live in different owners.
#[derive(Clone)]
pub struct RemoteInput {
    session_id: u64,
    commands: mpsc::UnboundedSender<Frame>,
}

impl RemoteInput {
    pub fn send_input(&self, data: Vec<u8>) {
        let _ = self.commands.send(Frame::Input {
            session_id: self.session_id,
            data,
        });
    }

    pub fn send_resize(&self, cols: u16, rows: u16) {
        let _ = self.commands.send(Frame::Resize {
            session_id: self.session_id,
            cols,
            rows,
        });
    }
}

/// Connect to a remote host over the relay (IK, paired device) and attach a
/// session — either a freshly opened one or an existing `session_id`. One
/// relay connection per session keeps the client trivially correct; the relay
/// already supports many client connections per host.
///
/// Runs the async pump on a dedicated thread so callers stay synchronous.
pub fn open_remote_session(
    relay_url: String,
    host_id: String,
    host_public_key: Vec<u8>,
    device: StaticKeypair,
    target: AttachTarget,
) -> Result<RemoteSession, NetError> {
    let (ready_tx, ready_rx) = std_mpsc::channel();
    let (output_tx, output_rx) = std_mpsc::channel();
    let (command_tx, command_rx) = mpsc::unbounded_channel();

    thread::Builder::new()
        .name("remote-client".into())
        .spawn(move || {
            let runtime = match RuntimeBuilder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx.send(Err(NetError::Internal(e.to_string())));
                    return;
                }
            };
            runtime.block_on(session_thread(
                relay_url,
                host_id,
                host_public_key,
                device,
                target,
                ready_tx,
                output_tx,
                command_rx,
            ));
        })
        .map_err(|e| NetError::Internal(e.to_string()))?;

    // The thread bounds its own waits, so this only has to outlast them; a
    // hard bound here is what keeps a wedged connect from parking the caller.
    let snapshot = ready_rx
        .recv_timeout(NET_TIMEOUT * 2)
        .map_err(|_| NetError::Timeout)??;
    Ok(RemoteSession {
        session_id: snapshot.session_id,
        snapshot,
        output: output_rx,
        commands: command_tx,
    })
}

pub enum AttachTarget {
    Open(ProtocolSessionOptions),
    Existing(u64),
}

/// How many times a dropped connection is re-established before the tab is
/// declared dead. The host keeps the shell running across disconnects, so
/// retrying is what makes a flaky link survivable rather than session-ending.
const RECONNECT_ATTEMPTS: u32 = 5;
const RECONNECT_BACKOFF: Duration = Duration::from_secs(2);

#[allow(clippy::too_many_arguments)]
async fn session_thread(
    relay_url: String,
    host_id: String,
    host_public_key: Vec<u8>,
    device: StaticKeypair,
    target: AttachTarget,
    ready: std_mpsc::Sender<Result<ProtocolSessionSnapshot, NetError>>,
    output: std_mpsc::Sender<SessionByteEvent>,
    mut commands: mpsc::UnboundedReceiver<Frame>,
) {
    let (mut channel, snapshot) =
        match connect_and_attach(&relay_url, &host_id, &host_public_key, &device, target).await {
            Ok(attached) => attached,
            Err(e) => {
                let _ = ready.send(Err(e));
                return;
            }
        };

    let session_id = snapshot.session_id;
    // Everything up to and including `base_seq` is already in the snapshot the
    // caller renders, so the pump only forwards events past it. After a resume
    // this is what suppresses the replay the fresh checkpoint already covers.
    let mut resume_after = snapshot.base_seq;
    if ready.send(Ok(snapshot)).is_err() {
        return; // Caller gave up before we finished attaching.
    }

    loop {
        match pump(channel, &output, &mut commands, resume_after).await {
            PumpExit::Local => return,
            PumpExit::SessionEnded => {
                let _ = output.send(SessionByteEvent::Exited);
                return;
            }
            PumpExit::Disconnected => {}
        }

        let Some((resumed, snapshot)) = reconnect(
            &relay_url,
            &host_id,
            &host_public_key,
            &device,
            session_id,
            &commands,
        )
        .await
        else {
            let _ = output.send(SessionByteEvent::Exited);
            return;
        };

        // The checkpoint replaces whatever the screen held: feeding it as
        // output lets the engine resync without any reconnect-aware code in
        // the terminal layer.
        if output.send(SessionByteEvent::Output(snapshot.vt)).is_err() {
            return;
        }
        resume_after = snapshot.base_seq;
        channel = resumed;
    }
}

/// Open a relay connection and attach, yielding the channel plus the snapshot
/// the caller starts rendering from.
async fn connect_and_attach(
    relay_url: &str,
    host_id: &str,
    host_public_key: &[u8],
    device: &StaticKeypair,
    target: AttachTarget,
) -> Result<(FrameChannel, ProtocolSessionSnapshot), NetError> {
    let mut channel = client_connect_ik(relay_url, host_id, host_public_key, device).await?;

    let session_id = match target {
        AttachTarget::Existing(id) => id,
        AttachTarget::Open(options) => {
            with_timeout(channel.send_control(&HostBound::Open(options))).await?;
            match with_timeout(channel.recv_control::<ClientBound>()).await? {
                ClientBound::Opened { session_id } => session_id,
                ClientBound::Error { message, .. } => return Err(NetError::Protocol(message)),
                other => {
                    return Err(NetError::Protocol(format!(
                        "expected Opened, got {other:?}"
                    )));
                }
            }
        }
    };

    with_timeout(channel.send_control(&HostBound::Attach { session_id })).await?;
    match with_timeout(channel.recv_control::<ClientBound>()).await? {
        ClientBound::Attached(snapshot) => Ok((channel, snapshot)),
        ClientBound::Error { message, .. } => Err(NetError::Protocol(message)),
        other => Err(NetError::Protocol(format!(
            "expected Attached, got {other:?}"
        ))),
    }
}

/// Re-attach to a session whose connection dropped. `None` means the retries
/// ran out or the local side went away, both of which end the tab.
async fn reconnect(
    relay_url: &str,
    host_id: &str,
    host_public_key: &[u8],
    device: &StaticKeypair,
    session_id: u64,
    commands: &mpsc::UnboundedReceiver<Frame>,
) -> Option<(FrameChannel, ProtocolSessionSnapshot)> {
    for attempt in 1..=RECONNECT_ATTEMPTS {
        if commands.is_closed() {
            return None; // Tab closed while we were retrying.
        }
        time::sleep(RECONNECT_BACKOFF * attempt).await;
        match connect_and_attach(
            relay_url,
            host_id,
            host_public_key,
            device,
            AttachTarget::Existing(session_id),
        )
        .await
        {
            Ok(attached) => {
                info!(session_id, attempt, "resumed remote session");
                return Some(attached);
            }
            Err(error) => {
                // A host rejection (session killed, device revoked) will not
                // change on retry. Local failures and timeouts may recover.
                if let Some(message) = error.permanent_reconnect_reason() {
                    warn!(session_id, "remote session cannot be resumed: {message}");
                    return None;
                }
                warn!(session_id, attempt, "remote resume failed: {error}");
            }
        }
    }
    None
}

/// Why [`pump`] returned: the difference decides whether the session can be
/// resumed or the tab is finished.
enum PumpExit {
    /// The local side hung up (tab closed) or stopped consuming output.
    Local,
    /// The remote shell exited, or the host rejected the stream.
    SessionEnded,
    /// The transport died; the session itself may still be alive on the host.
    Disconnected,
}

/// Full-duplex loop: relay Output/Exited frames to the terminal engine, and
/// Input/Resize commands to the host. `resume_after` drops output the caller
/// already has from the attach snapshot.
async fn pump(
    channel: FrameChannel,
    output: &std_mpsc::Sender<SessionByteEvent>,
    commands: &mut mpsc::UnboundedReceiver<Frame>,
    resume_after: u64,
) -> PumpExit {
    let FrameChannel { ws, mut chan } = channel;
    let (mut sink, mut stream) = ws.split();
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(frame) = command else { return PumpExit::Local };
                let Ok(bytes) = frame.encode() else { continue };
                let Ok(ciphertext) = chan.seal(&bytes) else { return PumpExit::Disconnected };
                if sink.send(Message::Binary(ciphertext.into())).await.is_err() {
                    return PumpExit::Disconnected;
                }
            }
            msg = stream.next() => {
                let data = match msg {
                    Some(Ok(Message::Binary(data))) => data,
                    Some(Ok(Message::Close(_))) | None => return PumpExit::Disconnected,
                    Some(Ok(_)) => continue,
                    Some(Err(_)) => return PumpExit::Disconnected,
                };
                // A decrypt failure means the stream is desynchronized or
                // tampered with: the Noise channel is unusable from here on.
                let Ok(plaintext) = chan.open(&data) else { return PumpExit::Disconnected };
                match Frame::decode(&plaintext) {
                    Ok(Frame::Output { seq, data, .. }) => {
                        if seq <= resume_after {
                            continue;
                        }
                        if output.send(SessionByteEvent::Output(data)).is_err() {
                            return PumpExit::Local;
                        }
                    }
                    Ok(Frame::Exited { .. }) => return PumpExit::SessionEnded,
                    // Control replies to mid-session requests are not used by
                    // the byte-stream consumer; ignore rather than error.
                    Ok(_) => continue,
                    Err(_) => return PumpExit::Disconnected,
                }
            }
        }
    }
}

/// Redeem a pairing code from this device (blocking). On success the host has
/// stored this device's public key, so later `open_remote_session` (IK) calls
/// are authorized. The pairing connection is closed immediately afterwards.
pub fn pair_device(
    code: PairingCode,
    device: StaticKeypair,
    device_name: String,
) -> Result<(), NetError> {
    let (tx, rx) = std_mpsc::channel();
    thread::Builder::new()
        .name("remote-pair".into())
        .spawn(move || {
            // A runtime that fails to build is a local resource problem, not a
            // peer rejection; report it instead of panicking the worker thread
            // (which would surface to the caller as an opaque Closed).
            let runtime = match RuntimeBuilder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(NetError::Internal(e.to_string())));
                    return;
                }
            };
            let result = runtime.block_on(async {
                crate::client_connect_pair(&code, &device, &device_name)
                    .await
                    .map(|_| ())
            });
            let _ = tx.send(result);
        })
        .map_err(|e| NetError::Internal(e.to_string()))?;
    rx.recv().map_err(|_| NetError::Closed)?
}

/// One-shot session listing over its own short-lived connection.
pub fn list_remote_sessions(
    relay_url: String,
    host_id: String,
    host_public_key: Vec<u8>,
    device: StaticKeypair,
) -> Result<Vec<ProtocolSessionInfo>, NetError> {
    let (tx, rx) = std_mpsc::channel();
    thread::Builder::new()
        .name("remote-list".into())
        .spawn(move || {
            // A runtime that fails to build is a local resource problem, not a
            // peer rejection; report it instead of panicking the worker thread
            // (which would surface to the caller as an opaque Closed).
            let runtime = match RuntimeBuilder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(NetError::Internal(e.to_string())));
                    return;
                }
            };
            let result = runtime.block_on(async {
                let mut channel =
                    client_connect_ik(&relay_url, &host_id, &host_public_key, &device).await?;
                channel.send_control(&HostBound::ListSessions).await?;
                // Bound the wait so a silent host can't hang the caller. A
                // silent host is indistinguishable from a slow one, so this is
                // Timeout (retryable), not a protocol violation.
                match time::timeout(Duration::from_secs(10), channel.recv_control()).await {
                    Ok(Ok(ClientBound::SessionList(list))) => Ok(list),
                    Ok(Ok(other)) => Err(NetError::Protocol(format!(
                        "expected SessionList, got {other:?}"
                    ))),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(NetError::Timeout),
                }
            });
            let _ = tx.send(result);
        })
        .map_err(|e| NetError::Internal(e.to_string()))?;
    rx.recv().map_err(|_| NetError::Closed)?
}
