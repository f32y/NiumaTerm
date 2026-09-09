//! The Remote WebSocket carries independent log, control, and interaction streams.

use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tracing::warn;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Error, Message, WebSocket, connect};

use crate::deepseek::api::{ApiClient, CallError};
use crate::deepseek::events::streams::Streams;
use crate::deepseek::host::Host;

mod streams;

#[cfg(test)]
mod tests;

const RECONNECT_DELAY: Duration = Duration::from_millis(500);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(200);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

pub(crate) struct Downlinks {
    stopped: Arc<AtomicBool>,
}

impl Downlinks {
    /// Wait for the event generation, control baseline, and log snapshot before
    /// admitting prompts. A successful socket upgrade alone does not establish
    /// the Host listeners and can otherwise lose the first turn's events.
    pub(crate) fn open(
        client: ApiClient,
        host: Weak<Host>,
        session_id: String,
        deliver: Arc<dyn Fn(Value) + Send + Sync>,
    ) -> Result<(Self, Value), String> {
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stopped = Arc::clone(&stopped);
        let (connected_tx, connected) = mpsc::channel();
        thread::spawn(move || {
            let mut connected_tx = Some(connected_tx);
            while !worker_stopped.load(Ordering::Relaxed) {
                let result = read_downlink(
                    &client,
                    &session_id,
                    deliver.as_ref(),
                    &worker_stopped,
                    &mut connected_tx,
                );
                if let Err(message) = result {
                    if let Some(sender) = connected_tx.take() {
                        let _ = sender.send(Err(message));
                        return;
                    }
                    if !worker_stopped.load(Ordering::Relaxed) {
                        warn!("deepseek stream disconnected: {message}");
                    }
                }
                if worker_stopped.load(Ordering::Relaxed)
                    || !host.upgrade().is_some_and(|host| host.is_running())
                {
                    return;
                }
                deliver(json!({ "payload": {
                    "type": "nmt/connection-reset", "sessionId": session_id,
                } }));
                thread::sleep(RECONNECT_DELAY);
            }
        });

        match connected.recv_timeout(CONNECT_TIMEOUT) {
            Ok(Ok(snapshot)) => Ok((Self { stopped }, snapshot)),
            outcome => {
                stopped.store(true, Ordering::Relaxed);
                Err(match outcome {
                    Ok(Err(message)) => message,
                    Err(_) => "the harness streams did not become ready in time".to_string(),
                    Ok(Ok(_)) => unreachable!(),
                })
            }
        }
    }
}

impl Drop for Downlinks {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
    }
}

fn read_downlink(
    client: &ApiClient,
    session_id: &str,
    deliver: &dyn Fn(Value),
    stopped: &AtomicBool,
    connected: &mut Option<mpsc::Sender<Result<Value, String>>>,
) -> Result<(), String> {
    let (mut socket, _) = connect(client.stream_request()?).map_err(|error| error.to_string())?;
    for (id, endpoint, args) in [
        ("events", "$events", json!({})),
        ("control", "session/control", json!({})),
        (
            "follow",
            "session/follow",
            follow_args(session_address(session_id), 200),
        ),
    ] {
        open_stream(&mut socket, id, endpoint, args)?;
    }
    let mut streams = Streams::new(session_id);
    pump(&mut socket, stopped, None, |frame| {
        streams.process(frame, client, deliver)?;
        if let Some(snapshot) = streams.ready_snapshot()
            && let Some(sender) = connected.take()
        {
            let _ = sender.send(Ok(snapshot.clone()));
        }
        Ok(())
    })
}

pub(crate) fn session_address(session_id: &str) -> Value {
    json!({ "kind": "session", "sessionId": session_id })
}

fn follow_args(address: Value, max_messages: u64) -> Value {
    json!({ "request": { "address": address, "maxMessages": max_messages } })
}

fn open_stream(socket: &mut Socket, id: &str, endpoint: &str, args: Value) -> Result<(), String> {
    socket
        .send(Message::Text(
            json!({
                "type": "open", "streamId": id, "endpoint": endpoint,
                "payload": { "args": args },
            })
            .to_string()
            .into(),
        ))
        .map_err(|error| error.to_string())
}

/// The follow opening is a consistent, cold-readable history window with its
/// projection cursor. Closing immediately after it avoids maintaining another
/// subscription for child transcripts or branch-point pickers.
pub(crate) fn snapshot(
    client: &ApiClient,
    address: Value,
    max_messages: u64,
) -> Result<Value, CallError> {
    let read = || -> Result<Value, String> {
        let (mut socket, _) =
            connect(client.stream_request()?).map_err(|error| error.to_string())?;
        set_poll_interval(&mut socket)?;
        open_stream(
            &mut socket,
            "snapshot",
            "session/follow",
            follow_args(address, max_messages),
        )?;
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        while Instant::now() < deadline {
            match read_message(&mut socket)? {
                Some(frame) if frame["type"] == "item" && frame["value"]["type"] == "snapshot" => {
                    let _ = socket.close(None);
                    return Ok(frame["value"].clone());
                }
                Some(frame) if frame["type"] == "error" => {
                    return Err(frame["error"]["message"]
                        .as_str()
                        .unwrap_or("history read failed")
                        .to_string());
                }
                _ => {}
            }
        }
        Err("the harness history snapshot did not arrive in time".to_string())
    };
    read().map_err(CallError::Transport)
}

fn set_poll_interval(socket: &mut Socket) -> Result<(), String> {
    if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
        stream
            .set_read_timeout(Some(STOP_POLL_INTERVAL))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn read_message(socket: &mut Socket) -> Result<Option<Value>, String> {
    match socket.read() {
        Ok(Message::Text(text)) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| error.to_string()),
        Ok(Message::Close(_)) => Err("the harness closed the stream".to_string()),
        // Reading queues Ping replies; flush them before an idle read timeout
        // so the Host's heartbeat does not discard a healthy connection.
        Ok(Message::Ping(_)) => socket
            .flush()
            .map(|_| None)
            .map_err(|error| error.to_string()),
        Ok(_) => Ok(None),
        Err(Error::Io(error))
            if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.to_string()),
    }
}

fn pump(
    socket: &mut Socket,
    stopped: &AtomicBool,
    read_started: Option<&mpsc::Sender<()>>,
    mut deliver: impl FnMut(Value) -> Result<(), String>,
) -> Result<(), String> {
    set_poll_interval(socket)?;
    if let Some(sender) = read_started {
        let _ = sender.send(());
    }
    while !stopped.load(Ordering::Relaxed) {
        if let Some(frame) = read_message(socket)? {
            deliver(frame)?;
        }
    }
    let _ = socket.close(None);
    Ok(())
}

#[cfg(test)]
pub(crate) fn pump_for_test(
    mut socket: Socket,
    deliver: &impl Fn(Value),
    stopped: &AtomicBool,
    read_started: &mpsc::Sender<()>,
) {
    let _ = pump(&mut socket, stopped, Some(read_started), |frame| {
        deliver(frame);
        Ok(())
    });
}
