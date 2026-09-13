//! The Remote WebSocket carries independent log, control, and interaction streams.

#[cfg(test)]
#[path = "events_tests.rs"]
mod events_tests;

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use reqwest::Url;
use serde_json::{Value, json};
use tracing::warn;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Error, Message, WebSocket, client, connect};

use crate::dsh::api::{ApiClient, CallError};
use crate::dsh::host::Host;

const RECONNECT_DELAY: Duration = Duration::from_millis(500);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(200);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

pub(crate) struct Downlinks {
    control: Arc<DownlinkControl>,
    worker: Option<thread::JoinHandle<()>>,
}

#[derive(Default)]
struct DownlinkControl {
    stopped: AtomicBool,
    socket: Mutex<Option<TcpStream>>,
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
        let (downlinks, connected) = Self::spawn(client, host, session_id, deliver)?;

        match connected.recv_timeout(CONNECT_TIMEOUT) {
            Ok(Ok(snapshot)) => Ok((downlinks, snapshot)),
            Ok(Err(message)) => Err(message),
            Err(_) => Err("the harness streams did not become ready in time".to_string()),
        }
    }

    fn spawn(
        client: ApiClient,
        host: Weak<Host>,
        session_id: String,
        deliver: Arc<dyn Fn(Value) + Send + Sync>,
    ) -> Result<(Self, mpsc::Receiver<Result<Value, String>>), String> {
        let control = Arc::new(DownlinkControl::default());
        let worker_control = Arc::clone(&control);
        let (connected_tx, connected) = mpsc::channel();

        let worker = thread::Builder::new()
            .name("deepseek-downlink".into())
            .spawn(move || {
                run_downlink(
                    client,
                    host,
                    session_id,
                    deliver,
                    worker_control,
                    connected_tx,
                )
            })
            .map_err(|error| error.to_string())?;

        Ok((
            Self {
                control,
                worker: Some(worker),
            },
            connected,
        ))
    }
}

fn run_downlink(
    client: ApiClient,
    host: Weak<Host>,
    session_id: String,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
    control: Arc<DownlinkControl>,
    connected_tx: mpsc::Sender<Result<Value, String>>,
) {
    let mut connected_tx = Some(connected_tx);

    while !control.stopped.load(Ordering::Relaxed) {
        let result = read_downlink(
            &client,
            &session_id,
            deliver.as_ref(),
            &control,
            &mut connected_tx,
        );

        if let Err(message) = result {
            if let Some(sender) = connected_tx.take() {
                let _ = sender.send(Err(message));

                return;
            }

            if !control.stopped.load(Ordering::Relaxed) {
                warn!("deepseek stream disconnected: {message}");
            }
        }

        if control.stopped.load(Ordering::Relaxed)
            || !host.upgrade().is_some_and(|host| host.is_running())
        {
            return;
        }

        deliver(json!({ "payload": {
            "type": "nmt/connection-reset", "sessionId": session_id,
        } }));

        thread::park_timeout(RECONNECT_DELAY);
    }
}

impl Drop for Downlinks {
    fn drop(&mut self) {
        self.control.stopped.store(true, Ordering::Relaxed);

        if let Some(socket) = self.control.socket.lock().take() {
            let _ = socket.shutdown(Shutdown::Both);
        }

        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();

            if worker.join().is_err() {
                warn!("deepseek downlink thread panicked");
            }
        }
    }
}

fn connect_downlink(api: &ApiClient, control: &DownlinkControl) -> Result<Socket, String> {
    let request = api.stream_request()?;
    let url = Url::parse(&request.uri().to_string()).map_err(|error| error.to_string())?;

    if url.scheme() != "ws" {
        return Err("the local harness requires a ws address".into());
    }

    let addresses = url
        .socket_addrs(|| Some(80))
        .map_err(|error| error.to_string())?;

    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut last_error = "the harness address has no usable socket".to_string();

    for address in addresses {
        loop {
            if control.stopped.load(Ordering::Relaxed) {
                return Err("the harness stream was stopped".into());
            }

            let remaining = deadline.saturating_duration_since(Instant::now());

            if remaining.is_zero() {
                return Err(last_error);
            }

            match TcpStream::connect_timeout(&address, remaining.min(STOP_POLL_INTERVAL)) {
                Ok(stream) => {
                    stream
                        .set_read_timeout(Some(remaining))
                        .map_err(|error| error.to_string())?;

                    stream
                        .set_write_timeout(Some(remaining))
                        .map_err(|error| error.to_string())?;

                    let mut registered = control.socket.lock();

                    if control.stopped.load(Ordering::Relaxed) {
                        return Err("the harness stream was stopped".into());
                    }

                    *registered = Some(stream.try_clone().map_err(|error| error.to_string())?);

                    drop(registered);

                    return client(request, MaybeTlsStream::Plain(stream))
                        .map(|(socket, _)| socket)
                        .map_err(|error| error.to_string());
                }

                Err(error) => {
                    last_error = error.to_string();

                    if error.kind() != ErrorKind::TimedOut {
                        break;
                    }
                }
            }
        }
    }

    Err(last_error)
}

fn read_downlink(
    client: &ApiClient,
    session_id: &str,
    deliver: &dyn Fn(Value),
    control: &DownlinkControl,
    connected: &mut Option<mpsc::Sender<Result<Value, String>>>,
) -> Result<(), String> {
    let mut socket = connect_downlink(client, control)?;

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

    pump(&mut socket, &control.stopped, None, |frame| {
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

// Decode logical streams into the adapter's local delivery messages.

struct Streams {
    session_id: String,
    client_id: Option<String>,
    control_ready: bool,
    snapshot: Option<Value>,
    pending: HashMap<String, &'static str>,
}

impl Streams {
    fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            client_id: None,
            control_ready: false,
            snapshot: None,
            pending: HashMap::new(),
        }
    }

    fn ready_snapshot(&self) -> Option<&Value> {
        if self.client_id.is_some() && self.control_ready {
            self.snapshot.as_ref()
        } else {
            None
        }
    }

    fn process(
        &mut self,
        frame: Value,
        client: &ApiClient,
        deliver: &dyn Fn(Value),
    ) -> Result<(), String> {
        let id = frame["streamId"].as_str().unwrap_or_default();

        match frame["type"].as_str() {
            Some("error") => {
                return Err(format!(
                    "{id}: {}",
                    frame["error"]["message"]
                        .as_str()
                        .unwrap_or("stream failed")
                ));
            }

            Some("end") => return Err(format!("the harness ended the {id} stream")),
            Some("item") => {}
            _ => return Err("the harness sent an invalid stream message".to_string()),
        }

        let value = &frame["value"];

        match id {
            "events" => self.on_event(value, client, deliver)?,
            "control" => self.on_control(value, deliver),

            "follow" => self.on_follow(value, deliver),

            _ => {}
        }

        Ok(())
    }

    fn on_follow(&mut self, value: &Value, deliver: &dyn Fn(Value)) {
        match value["type"].as_str() {
            Some("snapshot") => {
                deliver(json!({ "payload": {
                                "type": "nmt/replay", "sessionId": self.session_id, "page": value,
                            } }));

                self.snapshot = Some(value.clone());
            }

            Some("event") => deliver(json!({ "payload": {
                            "type": "session/event", "sessionId": self.session_id, "event": value["event"],
                        } })),

            _ => {}
        }
    }

    fn on_control(&mut self, value: &Value, deliver: &dyn Fn(Value)) {
        match value["type"].as_str() {
            Some("baseline") => {
                let baseline = &value["value"];

                self.queue(&baseline["queues"][&self.session_id], deliver);

                let projections = &baseline["projections"][&self.session_id];

                for (key, value) in projections["values"].as_object().into_iter().flatten() {
                    self.projection(key, value, &projections["asOfSeq"], deliver);
                }

                self.control_ready = true;
            }

            Some("queue") if value["sessionId"] == self.session_id => {
                self.queue(&value["items"], deliver)
            }

            Some("projection") if value["sessionId"] == self.session_id => {
                if let Some(key) = value["key"].as_str() {
                    self.projection(key, &value["value"], &value["seq"], deliver);
                }
            }

            _ => {}
        }
    }

    fn queue(&self, items: &Value, deliver: &dyn Fn(Value)) {
        deliver(json!({ "payload": {
            "type": "session/queue", "sessionId": self.session_id, "items": items,
        } }));
    }

    fn projection(&self, key: &str, value: &Value, seq: &Value, deliver: &dyn Fn(Value)) {
        deliver(json!({ "payload": {
            "type": "session/projection", "sessionId": self.session_id,
            "key": key, "value": value, "seq": seq,
        } }));
    }

    fn on_event(
        &mut self,
        value: &Value,
        client: &ApiClient,
        deliver: &dyn Fn(Value),
    ) -> Result<(), String> {
        match value["type"].as_str() {
            Some("ready") => {
                self.client_id = Some(
                    value["clientId"]
                        .as_str()
                        .ok_or("the harness event stream supplied no generation id")?
                        .to_string(),
                );
            }

            Some("emit")
                if value["event"] == "api-session/error" && value["args"][0] == self.session_id =>
            {
                deliver(json!({ "payload": {
                    "type": "host/agent-error", "sessionId": self.session_id, "message": value["args"][1],
                } }));
            }

            Some("waterfall") => {
                let client_id = self
                    .client_id
                    .as_deref()
                    .ok_or("an interaction arrived before event readiness")?;

                let event_id = value["eventId"]
                    .as_str()
                    .ok_or("an interaction arrived without an event id")?;

                let kind = match value["event"].as_str() {
                    Some("approval/request") => Some(("approval/requested", "approval/resolved")),

                    Some("user-questions/request") => {
                        Some(("question/requested", "question/resolved"))
                    }

                    _ => None,
                };

                if value["agentId"] != self.session_id || kind.is_none() {
                    client
                        .respond_event(client_id, event_id, json!({ "kind": "next" }))
                        .map_err(|error| error.message().to_string())?;

                    return Ok(());
                }

                let (requested, resolved) = kind.unwrap();
                let mut payload = value["request"].clone();

                payload["type"] = json!(requested);
                payload["sessionId"] = json!(self.session_id);

                // The pane holds one card of each kind. Successful answers do
                // not echo a cancellation to their sender, so replace the old
                // identity when the next card arrives instead of retaining it.
                self.pending.retain(|_, kind| *kind != resolved);
                self.pending.insert(event_id.to_string(), resolved);
                deliver(json!({ "clientId": client_id, "eventId": event_id, "payload": payload }));
            }

            Some("cancel") => {
                if let Some(event_id) = value["eventId"].as_str()
                    && let Some(kind) = self.pending.remove(event_id)
                {
                    deliver(
                        json!({ "clientId": self.client_id, "eventId": event_id, "payload": {
                        "type": kind, "sessionId": self.session_id,
                    } }),
                    );
                }
            }

            _ => {}
        }

        Ok(())
    }
}
