use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use nmt_remote_protocol::{
    ClientBound, Frame, HostBound, PairingCode, StaticKeypair, WireSessionInfo, WireSessionOptions,
    WireSessionSnapshot,
};
use tokio::runtime::Builder as RuntimeBuilder;
use tokio::sync::mpsc;
use tokio::time;
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::{FrameChannel, NetError, client_connect_ik};

/// One remote session's byte stream as the terminal engine wants to consume
/// it: opaque output bytes plus a terminal exit. Mirrors the hub's
/// `SessionEvent` but flattened to what a PTY reader yields.
#[derive(Debug)]
pub enum SessionByteEvent {
    Output(Vec<u8>),
    Exited,
}

/// Client-side handle to one attached remote session. Its receiver/senders are
/// std (not tokio) types so the synchronous terminal PTY seam (`NetPty`) can
/// use them directly without touching the async runtime.
pub struct RemoteSession {
    pub session_id: u64,
    pub snapshot: WireSessionSnapshot,
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

    pub fn snapshot(&self) -> &WireSessionSnapshot {
        &self.snapshot
    }

    /// A cloneable input handle usable independently of the output receiver, so
    /// the terminal PTY seam can own the byte stream while the UI still sends
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
                    let _ = ready_tx.send(Err(NetError::Protocol(e.to_string())));
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
        .map_err(|e| NetError::Protocol(e.to_string()))?;

    let snapshot = ready_rx.recv().map_err(|_| NetError::Closed)??;
    Ok(RemoteSession {
        session_id: snapshot.session_id,
        snapshot,
        output: output_rx,
        commands: command_tx,
    })
}

pub enum AttachTarget {
    Open(WireSessionOptions),
    Existing(u64),
}

#[allow(clippy::too_many_arguments)]
async fn session_thread(
    relay_url: String,
    host_id: String,
    host_public_key: Vec<u8>,
    device: StaticKeypair,
    target: AttachTarget,
    ready: std_mpsc::Sender<Result<WireSessionSnapshot, NetError>>,
    output: std_mpsc::Sender<SessionByteEvent>,
    commands: mpsc::UnboundedReceiver<Frame>,
) {
    let mut channel = match client_connect_ik(&relay_url, &host_id, &host_public_key, &device).await
    {
        Ok(c) => c,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };

    let session_id = match target {
        AttachTarget::Existing(id) => id,
        AttachTarget::Open(options) => {
            if let Err(e) = channel.send_control(&HostBound::Open(options)).await {
                let _ = ready.send(Err(e));
                return;
            }
            match channel.recv_control::<ClientBound>().await {
                Ok(ClientBound::Opened { session_id }) => session_id,
                Ok(ClientBound::Error { message, .. }) => {
                    let _ = ready.send(Err(NetError::Protocol(message)));
                    return;
                }
                Ok(other) => {
                    let _ = ready.send(Err(NetError::Protocol(format!(
                        "expected Opened, got {other:?}"
                    ))));
                    return;
                }
                Err(e) => {
                    let _ = ready.send(Err(e));
                    return;
                }
            }
        }
    };

    if let Err(e) = channel
        .send_control(&HostBound::Attach { session_id })
        .await
    {
        let _ = ready.send(Err(e));
        return;
    }
    let snapshot = match channel.recv_control::<ClientBound>().await {
        Ok(ClientBound::Attached(snapshot)) => snapshot,
        Ok(ClientBound::Error { message, .. }) => {
            let _ = ready.send(Err(NetError::Protocol(message)));
            return;
        }
        Ok(other) => {
            let _ = ready.send(Err(NetError::Protocol(format!(
                "expected Attached, got {other:?}"
            ))));
            return;
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    if ready.send(Ok(snapshot)).is_err() {
        return; // Caller gave up before we finished attaching.
    }

    pump(channel, output, commands).await;
}

/// Full-duplex loop: relay Output/Exited frames to the terminal engine, and
/// Input/Resize commands to the host. Exits when either side closes.
async fn pump(
    channel: FrameChannel,
    output: std_mpsc::Sender<SessionByteEvent>,
    mut commands: mpsc::UnboundedReceiver<Frame>,
) {
    let FrameChannel { ws, mut chan } = channel;
    let (mut sink, mut stream) = ws.split();
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(frame) = command else { return };
                let Ok(bytes) = frame.encode() else { continue };
                let Ok(ciphertext) = chan.seal(&bytes) else { return };
                if sink.send(Message::Binary(ciphertext.into())).await.is_err() {
                    return;
                }
            }
            msg = stream.next() => {
                let data = match msg {
                    Some(Ok(Message::Binary(data))) => data,
                    Some(Ok(Message::Close(_))) | None => {
                        let _ = output.send(SessionByteEvent::Exited);
                        return;
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(_)) => {
                        let _ = output.send(SessionByteEvent::Exited);
                        return;
                    }
                };
                let Ok(plaintext) = chan.open(&data) else { return };
                match Frame::decode(&plaintext) {
                    Ok(Frame::Output { data, .. }) => {
                        if output.send(SessionByteEvent::Output(data)).is_err() {
                            return;
                        }
                    }
                    Ok(Frame::Exited { .. }) => {
                        let _ = output.send(SessionByteEvent::Exited);
                        return;
                    }
                    // Control replies to mid-session requests are not used by
                    // the byte-stream consumer; ignore rather than error.
                    Ok(_) => continue,
                    Err(_) => return,
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
            let runtime = RuntimeBuilder::new_current_thread()
                .enable_all()
                .build()
                .expect("current-thread runtime");
            let result = runtime.block_on(async {
                crate::client_connect_pair(&code, &device, &device_name)
                    .await
                    .map(|_| ())
            });
            let _ = tx.send(result);
        })
        .map_err(|e| NetError::Protocol(e.to_string()))?;
    rx.recv().map_err(|_| NetError::Closed)?
}

/// One-shot session listing over its own short-lived connection.
pub fn list_remote_sessions(
    relay_url: String,
    host_id: String,
    host_public_key: Vec<u8>,
    device: StaticKeypair,
) -> Result<Vec<WireSessionInfo>, NetError> {
    let (tx, rx) = std_mpsc::channel();
    thread::Builder::new()
        .name("remote-list".into())
        .spawn(move || {
            let runtime = RuntimeBuilder::new_current_thread()
                .enable_all()
                .build()
                .expect("current-thread runtime");
            let result = runtime.block_on(async {
                let mut channel =
                    client_connect_ik(&relay_url, &host_id, &host_public_key, &device).await?;
                channel.send_control(&HostBound::ListSessions).await?;
                // Bound the wait so a silent host can't hang the caller.
                match time::timeout(Duration::from_secs(10), channel.recv_control()).await {
                    Ok(Ok(ClientBound::SessionList(list))) => Ok(list),
                    Ok(Ok(other)) => Err(NetError::Protocol(format!(
                        "expected SessionList, got {other:?}"
                    ))),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(NetError::Protocol("host did not reply in time".into())),
                }
            });
            let _ = tx.send(result);
        })
        .map_err(|e| NetError::Protocol(e.to_string()))?;
    rx.recv().map_err(|_| NetError::Closed)?
}
