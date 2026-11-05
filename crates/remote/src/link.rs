//! Moves binary messages between a WebSocket and the protocol state machines.
//! One WebSocket binary message holds one Noise message, on the LAN and
//! later through the relay, so both paths share this code.

use std::sync::Arc;
#[cfg(any(feature = "host", test))]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use std::{error, fmt};

use anyhow::{Result, bail};
use futures::{Sink, SinkExt as _, Stream, StreamExt as _};
use nmt_remote_core::channel::Channel;
use nmt_remote_core::frame::{
    CONTROL_STREAM, Frame, Message as FrameMessage, Reassembler, encode_message, kind,
};
use tokio::select;
#[cfg(any(feature = "host", test))]
use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::{Instant, sleep_until};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tracing::info;

pub(crate) const AUTH_REFUSED_CODE: u16 = 4403;

#[derive(Debug)]
pub(crate) enum ReceiveEnd {
    Close(Option<CloseFrame>),
    StreamEnd,
}

impl fmt::Display for ReceiveEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Close(Some(frame)) => write!(
                f,
                "WebSocket closed with code {}: {}",
                u16::from(frame.code),
                frame.reason
            ),
            Self::Close(None) => f.write_str("WebSocket closed without a status code"),
            Self::StreamEnd => f.write_str("WebSocket stream ended without a close frame"),
        }
    }
}

impl error::Error for ReceiveEnd {}

/// Largest backlog one channel may queue. A peer that stays this far
/// behind is closed and reconnects, which replaces the backlog with a
/// checkpoint instead of letting memory and latency grow without bound.
pub(crate) const MAX_QUEUED: usize = 8 * 1024 * 1024;

#[cfg(any(feature = "host", test))]
const BOUNDED_QUEUED: usize = MAX_QUEUED / 2;

const CONTROL_CHUNK_SIZE: usize = 128 * 1024;
const CONTROL_CHUNK_HEADER: usize = 24;
pub(crate) const MAX_CONTROL_TRANSFER: usize = 64 * 1024 * 1024;

/// Reassemble one bounded control transfer. A new first chunk replaces an
/// interrupted transfer; subsequent chunks must match its id and offset.
#[derive(Default)]
pub(crate) struct ControlChunks {
    transfer: Option<ControlTransfer>,
}

struct ControlTransfer {
    id: u64,
    total: usize,
    bytes: Vec<u8>,
}

impl ControlChunks {
    pub(crate) fn push(&mut self, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        if payload.len() <= CONTROL_CHUNK_HEADER
            || payload.len() > CONTROL_CHUNK_HEADER + CONTROL_CHUNK_SIZE
        {
            bail!("invalid control chunk length");
        }

        let id = u64::from_be_bytes(payload[..8].try_into()?);
        let total = usize::try_from(u64::from_be_bytes(payload[8..16].try_into()?))?;
        let offset = usize::try_from(u64::from_be_bytes(payload[16..24].try_into()?))?;
        let bytes = &payload[CONTROL_CHUNK_HEADER..];

        if total == 0 || total > MAX_CONTROL_TRANSFER || bytes.len() > total.saturating_sub(offset)
        {
            bail!("control transfer exceeds its declared size");
        }

        if offset == 0 {
            if self
                .transfer
                .as_ref()
                .is_some_and(|transfer| transfer.id == id)
            {
                bail!("control transfer repeated its first chunk");
            }

            self.transfer = Some(ControlTransfer {
                id,
                total,
                bytes: Vec::with_capacity(total),
            });
        }

        let Some(transfer) = self.transfer.as_mut() else {
            bail!("control chunk has no first chunk");
        };

        if transfer.id != id || transfer.total != total || transfer.bytes.len() != offset {
            bail!("control chunk is out of sequence");
        }

        transfer.bytes.extend_from_slice(bytes);

        if transfer.bytes.len() == transfer.total {
            return Ok(self.transfer.take().map(|transfer| transfer.bytes));
        }

        Ok(None)
    }
}

/// One message to send on a channel stream.
pub(crate) struct Outbound {
    pub stream: u32,
    pub kind: u8,
    pub payload: Vec<u8>,

    /// Told how many payload bytes left once the message is sent, so
    /// a stream can track its own backlog.
    pub on_sent: Option<Arc<dyn SentHook>>,
}

pub(crate) trait SentHook: Send + Sync {
    fn sent(&self, bytes: usize);
}

impl Outbound {
    pub(crate) fn new(stream: u32, kind: u8, payload: Vec<u8>) -> Self {
        Self {
            stream,
            kind,
            payload,
            on_sent: None,
        }
    }
}

/// The sending half of a channel's outbound queue, which counts the bytes
/// waiting in it.
#[derive(Clone)]
pub(crate) struct SendQueue {
    tx: UnboundedSender<Outbound>,
    queued: Arc<AtomicUsize>,
    drained: Arc<Notify>,
    #[cfg(any(feature = "host", test))]
    control_send: Arc<Mutex<()>>,
    #[cfg(any(feature = "host", test))]
    next_transfer: Arc<AtomicU64>,
}

pub(crate) struct QueueReceiver {
    rx: UnboundedReceiver<Outbound>,
    queued: Arc<AtomicUsize>,
    drained: Arc<Notify>,
}

impl QueueReceiver {
    #[cfg(test)]
    pub(crate) fn try_recv(&mut self) -> Option<Outbound> {
        let outbound = self.rx.try_recv().ok()?;

        self.queued
            .fetch_sub(outbound.payload.len(), Ordering::Relaxed);

        self.drained.notify_waiters();

        Some(outbound)
    }
}

impl SendQueue {
    #[cfg(test)]
    pub(crate) fn queued_bytes(&self) -> usize {
        self.queued.load(Ordering::Relaxed)
    }

    pub(crate) fn new() -> (Self, QueueReceiver) {
        let (tx, rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let drained = Arc::new(Notify::new());

        (
            Self {
                tx,
                queued: Arc::clone(&queued),
                drained: Arc::clone(&drained),
                #[cfg(any(feature = "host", test))]
                control_send: Arc::new(Mutex::new(())),
                #[cfg(any(feature = "host", test))]
                next_transfer: Arc::new(AtomicU64::new(1)),
            },
            QueueReceiver {
                rx,
                queued,
                drained,
            },
        )
    }

    /// Queue a message; `false` once the channel is gone.
    pub(crate) fn send(&self, outbound: Outbound) -> bool {
        let len = outbound.payload.len();

        self.queued.fetch_add(len, Ordering::Relaxed);

        if self.tx.send(outbound).is_err() {
            self.queued.fetch_sub(len, Ordering::Relaxed);
            self.drained.notify_waiters();

            return false;
        }

        true
    }

    /// Wait for capacity before adding one bounded message. Large logical
    /// responses must be split before admission so a slow reader cannot
    /// accumulate the complete response in the transport queue.
    #[cfg(any(feature = "host", test))]
    pub(crate) async fn send_bounded(&self, outbound: Outbound) -> bool {
        let len = outbound.payload.len();

        if len > BOUNDED_QUEUED {
            return false;
        }

        loop {
            let drained = self.drained.notified();

            tokio::pin!(drained);

            // Register before checking capacity so a concurrent drain cannot
            // complete between the check and notification registration.
            drained.as_mut().enable();

            if self.tx.is_closed() {
                return false;
            }

            if self
                .queued
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |queued| {
                    queued
                        .checked_add(len)
                        .filter(|total| *total <= BOUNDED_QUEUED)
                })
                .is_ok()
            {
                if self.tx.send(outbound).is_err() {
                    self.queued.fetch_sub(len, Ordering::Relaxed);
                    self.drained.notify_waiters();

                    return false;
                }

                return true;
            }

            select! {
                () = drained => {}
                () = self.tx.closed() => return false,
            }
        }
    }

    /// Serialize control responses in bounded chunks while retaining their
    /// order. The caller uses this only after negotiating chunk support.
    #[cfg(any(feature = "host", test))]
    pub(crate) async fn send_chunked_control(&self, outbound: Outbound) -> bool {
        if outbound.stream != CONTROL_STREAM
            || outbound.kind != kind::CONTROL_JSON
            || outbound.on_sent.is_some()
            || outbound.payload.len() > MAX_CONTROL_TRANSFER
        {
            return false;
        }

        let _sending = self.control_send.lock().await;

        if outbound.payload.len() <= CONTROL_CHUNK_SIZE {
            return self.send_bounded(outbound).await;
        }

        let id = self.next_transfer.fetch_add(1, Ordering::Relaxed);
        let total = outbound.payload.len() as u64;

        for (index, bytes) in outbound.payload.chunks(CONTROL_CHUNK_SIZE).enumerate() {
            let mut payload = Vec::with_capacity(CONTROL_CHUNK_HEADER + bytes.len());

            payload.extend_from_slice(&id.to_be_bytes());
            payload.extend_from_slice(&total.to_be_bytes());
            payload.extend_from_slice(&((index * CONTROL_CHUNK_SIZE) as u64).to_be_bytes());
            payload.extend_from_slice(bytes);

            if !self
                .send_bounded(Outbound::new(CONTROL_STREAM, kind::CONTROL_CHUNK, payload))
                .await
            {
                return false;
            }
        }

        true
    }

    #[cfg(feature = "host")]
    pub(crate) fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

/// A WebSocket on any transport: TCP on the LAN, TLS to the relay, or a
/// QUIC stream on the direct path. A client link switches between them, so
/// it keeps the socket boxed.
pub(crate) trait MessageSocket:
    Stream<Item = Result<Message, WsError>> + Sink<Message, Error = WsError> + Unpin + Send
{
}

impl<T> MessageSocket for T where
    T: Stream<Item = Result<Message, WsError>> + Sink<Message, Error = WsError> + Unpin + Send
{
}

pub(crate) type BoxSocket = Box<dyn MessageSocket>;

pub(crate) async fn recv_binary<S>(ws: &mut S) -> Result<Vec<u8>>
where
    S: Stream<Item = Result<Message, WsError>> + Unpin,
{
    loop {
        match ws.next().await {
            Some(Ok(Message::Binary(bytes))) => return Ok(bytes.to_vec()),
            Some(Ok(Message::Close(frame))) => {
                info!(origin = "websocket_close", code = ?frame.as_ref().map(|frame| u16::from(frame.code)),
                    reason = ?frame.as_ref().map(|frame| frame.reason.as_str()), "remote socket ended");

                return Err(ReceiveEnd::Close(frame).into());
            }
            None => {
                info!(origin = "stream_end", "remote socket ended");

                return Err(ReceiveEnd::StreamEnd.into());
            }
            Some(Ok(_)) => {}
            Some(Err(error)) => {
                info!(origin = "receive", error = %format_args!("{error:#}"), "remote socket failed");

                return Err(error.into());
            }
        }
    }
}

pub(crate) async fn send_binary<S>(ws: &mut S, bytes: Vec<u8>) -> Result<()>
where
    S: Sink<Message, Error = WsError> + Unpin,
{
    ws.send(Message::Binary(bytes.into()))
        .await
        .inspect_err(|error| {
            info!(origin = "send", error = %format_args!("{error:#}"), "remote socket failed");
        })?;

    Ok(())
}

/// Probe a quiet channel after this long without inbound frames.
const IDLE_BEFORE_PING: Duration = Duration::from_secs(30);

/// An unanswered probe counts as missed after this long.
const PING_TIMEOUT: Duration = Duration::from_secs(10);

/// Two missed probes in a row mark the channel dead, so one lost to a
/// momentary stall does not tear down working sessions.
const MAX_MISSED_PINGS: u32 = 2;

/// How long a probe asked for after a network change may go unanswered.
/// The change itself is the evidence that the socket may be dead, so one
/// miss closes the channel instead of the two a quiet link gets.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Run an established channel until either side closes it: seal and send
/// `outbound`, open and reassemble what arrives into `inbound`. One task owns
/// both directions because the Noise transport keeps both ciphers in one
/// state.
///
/// The pump also keeps the channel's liveness accurate: a network that
/// drops packets without closing the socket (sleep, a pulled cable, a NAT
/// timeout) would otherwise leave both sides waiting forever. It answers
/// probes itself, so the layers above never see them. `probe` asks for one
/// at once, as after the machine's network changed: a socket bound to an
/// address that is gone stays silent, and waiting out the idle timer would
/// leave the views frozen for most of a minute.
pub(crate) async fn pump<S>(
    mut ws: S,
    mut channel: Channel,
    mut outbound: QueueReceiver,
    inbound: UnboundedSender<FrameMessage>,
    probe: Arc<Notify>,
) -> Result<()>
where
    S: Stream<Item = Result<Message, WsError>> + Sink<Message, Error = WsError> + Unpin,
{
    let mut reassembler = Reassembler::default();
    let mut deadline = Instant::now() + IDLE_BEFORE_PING;
    let mut missed = 0;
    let mut probing = false;

    loop {
        select! {
            received = recv_binary(&mut ws) => {
                let plaintext = channel.open(&received?)?;
                let frame = Frame::decode(&plaintext)?;

                deadline = Instant::now() + IDLE_BEFORE_PING;
                missed = 0;
                probing = false;

                match (frame.stream, frame.kind) {
                    (CONTROL_STREAM, kind::PING) => {
                        send_frames(&mut ws, &mut channel, CONTROL_STREAM, kind::PONG, frame.payload)
                            .await?;
                    }
                    (CONTROL_STREAM, kind::PONG) => {}
                    _ => {
                        if let Some(message) = reassembler.push(frame)?
                            && inbound.send(message).is_err()
                        {
                            return Ok(());
                        }
                    }
                }
            }

            next = outbound.rx.recv() => {
                let Some(next) = next else {
                    info!(origin = "local", reason = "outbound_queue_closed", "closing remote socket");

                    let _ = ws.close().await;

                    return Ok(());
                };

                if outbound.queued.load(Ordering::Relaxed) > MAX_QUEUED {
                    bail!("the peer fell too far behind");
                }

                send_frames(&mut ws, &mut channel, next.stream, next.kind, &next.payload).await?;

                outbound
                    .queued
                    .fetch_sub(next.payload.len(), Ordering::Relaxed);

                outbound.drained.notify_waiters();

                if let Some(hook) = &next.on_sent {
                    hook.sent(next.payload.len());
                }
            }

            () = probe.notified() => {
                missed = MAX_MISSED_PINGS - 1;
                probing = true;
                deadline = Instant::now() + PROBE_TIMEOUT;

                send_frames(&mut ws, &mut channel, CONTROL_STREAM, kind::PING, &[]).await?;
            }

            () = sleep_until(deadline) => {
                if probing {
                    missed += 1;

                    if missed >= MAX_MISSED_PINGS {
                        info!(origin = "local", reason = "heartbeat_timeout", missed,
                            "closing remote socket");

                        bail!("the peer stopped answering");
                    }
                }

                probing = true;
                deadline = Instant::now() + PING_TIMEOUT;

                send_frames(&mut ws, &mut channel, CONTROL_STREAM, kind::PING, &[]).await?;
            }
        }
    }
}

async fn send_frames<S>(
    ws: &mut S,
    channel: &mut Channel,
    stream: u32,
    kind: u8,
    payload: &[u8],
) -> Result<()>
where
    S: Sink<Message, Error = WsError> + Unpin,
{
    for frame in encode_message(stream, kind, payload) {
        let sealed = channel.seal(&frame)?;

        send_binary(ws, sealed).await?;
    }

    Ok(())
}
