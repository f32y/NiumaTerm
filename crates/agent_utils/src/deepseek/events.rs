//! The two WebSocket downlinks the host serves: `events.mux` carries
//! per-session activity, `events.host` reports the host's own state, which
//! belongs to no turn.
//!
//! A plain GET on either answers 426, so these are true WebSocket upgrades
//! rather than the streaming-GET the in-process carrier uses. The client is the
//! synchronous one on a reader thread, matching how the other agent adapters
//! deliver protocol messages; introducing an async runtime for two long-lived
//! sockets would buy nothing.

use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::thread;
use std::time::Duration;

use serde_json::Value;
use tracing::warn;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::deepseek::host::Host;

/// Both downlink paths. They are opened together because a client that reads
/// only one sees session activity without knowing the session exists, or the
/// reverse.
const DOWNLINKS: [&str; 2] = ["/api/events.mux", "/api/events.host"];

/// A dropped socket is reopened rather than ending the tab: the host is
/// long-lived and a reconnect re-emits a baseline for every attached session,
/// so the client can rebuild without the tab noticing.
const RECONNECT_DELAY: Duration = Duration::from_millis(500);

/// Maximum time an idle socket may delay observing that its tab was closed.
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// How long opening a session waits for its downlinks. Both are loopback
/// upgrades against an already-serving host, so this only has to cover a busy
/// machine rather than a network.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Reader threads for both downlinks. Dropping this stops them: each loop
/// checks the shared flag between frames, and the sockets close with it.
pub(crate) struct Downlinks {
    stopped: Arc<AtomicBool>,
}

impl Downlinks {
    /// Open both downlinks and deliver every decoded frame to `deliver`.
    ///
    /// Frames are handed over as raw values. Deciding which are interesting
    /// belongs to the session that owns them, and a frame this build does not
    /// recognize has to survive the trip so it can be ignored deliberately
    /// rather than dropped by the transport.
    /// `host` is held weakly so these threads cannot keep the process alive
    /// past the last tab, and so a host that exited can be recognized as gone
    /// rather than reconnected to forever.
    pub(crate) fn open(
        base: &str,
        host: Weak<Host>,
        deliver: impl Fn(Value) + Send + Sync + 'static,
    ) -> Self {
        let stopped = Arc::new(AtomicBool::new(false));
        // Both reader threads feed the same pane, and requiring the caller to
        // hand over a cloneable closure would expose that there happen to be
        // two sockets here rather than one.
        let deliver = Arc::new(deliver);
        let (connected_tx, connected) = mpsc::channel();

        for path in DOWNLINKS {
            let url = websocket_url(base, path);
            let deliver = Arc::clone(&deliver);
            let stopped = Arc::clone(&stopped);
            let host = host.clone();
            let connected_tx = connected_tx.clone();
            thread::spawn(move || {
                read_downlink(&url, &host, deliver.as_ref(), &stopped, &connected_tx);
            });
        }
        drop(connected_tx);

        // Returning before both sockets are live would drop the opening frames
        // of whatever the caller does next: the stream replays a per-session
        // baseline on connect, but a turn that starts in the gap emits its
        // `turn/start` to nobody. Waiting here is what makes "the session is
        // ready" true rather than merely requested.
        for _ in 0..DOWNLINKS.len() {
            if connected.recv_timeout(CONNECT_TIMEOUT).is_err() {
                warn!("a deepseek downlink was not live before the wait expired");
                break;
            }
        }

        Self { stopped }
    }
}

impl Drop for Downlinks {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
    }
}

/// Reconnect until asked to stop or until the host is gone. Each successful
/// open replays the baseline the host emits for every attached session, so no
/// sequence bookkeeping is needed on this side to recover from a drop.
///
/// Returning drops this thread's share of `deliver`. Once both threads return,
/// the channel it writes to closes, which is the same end-of-stream a CLI
/// backend's dying process produces and reaches the same handling.
fn read_downlink(
    url: &str,
    host: &Weak<Host>,
    deliver: &impl Fn(Value),
    stopped: &AtomicBool,
    connected: &mpsc::Sender<()>,
) {
    while !stopped.load(Ordering::Relaxed) {
        match tungstenite::connect(url) {
            Ok((socket, _)) => {
                // Only the first connect is awaited; a reconnect happens while
                // the session is already running and has nobody waiting on it.
                let _ = connected.send(());
                pump(socket, deliver, stopped);
            }
            Err(error) => warn!("deepseek downlink {url} could not be opened: {error}"),
        }

        // A dropped socket is normal and reconnects; a dropped socket with no
        // host behind it any more is the end of every session it served.
        match host.upgrade() {
            Some(host) if host.is_running() => {}
            _ => return,
        }

        if stopped.load(Ordering::Relaxed) {
            return;
        }
        thread::sleep(RECONNECT_DELAY);
    }
}

fn pump(
    mut socket: WebSocket<MaybeTlsStream<TcpStream>>,
    deliver: &impl Fn(Value),
    stopped: &AtomicBool,
) {
    pump_with_read_signal(&mut socket, deliver, stopped, None);
}

#[cfg(test)]
pub(crate) fn pump_for_test(
    mut socket: WebSocket<MaybeTlsStream<TcpStream>>,
    deliver: &impl Fn(Value),
    stopped: &AtomicBool,
    read_started: &mpsc::Sender<()>,
) {
    pump_with_read_signal(&mut socket, deliver, stopped, Some(read_started));
}

fn pump_with_read_signal(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    deliver: &impl Fn(Value),
    stopped: &AtomicBool,
    read_started: Option<&mpsc::Sender<()>>,
) {
    if let MaybeTlsStream::Plain(stream) = socket.get_mut()
        && let Err(error) = stream.set_read_timeout(Some(STOP_POLL_INTERVAL))
    {
        warn!("deepseek downlink could not set its close poll interval: {error}");
    }

    let mut announced_read = false;
    while !stopped.load(Ordering::Relaxed) {
        if !announced_read && let Some(read_started) = read_started {
            let _ = read_started.send(());
            announced_read = true;
        }
        match socket.read() {
            // Only text frames carry protocol messages. Ping and pong are
            // answered by the library's own write path, and a binary frame is
            // not something this interface produces.
            Ok(Message::Text(text)) => match serde_json::from_str::<Value>(&text) {
                Ok(frame) => deliver(frame),
                Err(error) => warn!("deepseek downlink sent an unreadable frame: {error}"),
            },
            Ok(Message::Close(_)) => return,
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {}
            Err(error) => {
                warn!("deepseek downlink ended: {error}");
                return;
            }
        }
    }

    let _ = socket.close(None);
}

fn websocket_url(base: &str, path: &str) -> String {
    let authority = base
        .strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"))
        .unwrap_or(base);
    let scheme = if base.starts_with("https://") {
        "wss"
    } else {
        "ws"
    };

    format!("{scheme}://{authority}{path}")
}
