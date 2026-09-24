//! The Remote WebSocket carries independent log, control, and interaction streams.

#[cfg(test)]
#[path = "events_tests.rs"]
mod events_tests;

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

use futures::{SinkExt as _, StreamExt as _};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio::task::AbortHandle;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tracing::warn;
use tungstenite::{Error, Message};

use crate::dsh::api::{ApiClient, CallError};
use crate::dsh::host::Host;
use crate::dsh::session::SESSION_STATUS;

const RECONNECT_DELAY: Duration = Duration::from_millis(500);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

type Delivery = Arc<dyn Fn(Value) + Send + Sync>;

pub(crate) struct Downlinks {
    reader: AbortHandle,
    gate: DeliveryGate,
}

/// The reader's only path to the tab. A delivery runs under the gate's lock,
/// and closing takes the callback out under that lock, so once closing returns
/// no frame can reach the tab and the callback has been released. Closing
/// waits only for a delivery already under way, never for the reader task.
#[derive(Clone)]
struct DeliveryGate(Arc<Mutex<Option<Delivery>>>);

impl DeliveryGate {
    fn deliver(&self, frame: Value) {
        if let Some(deliver) = self.0.lock().as_ref() {
            deliver(frame);
        }
    }

    fn close(&self) -> Option<Delivery> {
        self.0.lock().take()
    }
}

impl Downlinks {
    /// Wait for the event generation, control baseline, and log snapshot before
    /// admitting prompts. A successful socket upgrade alone does not establish
    /// the Host listeners and can otherwise lose the first turn's events.
    pub(crate) async fn open(
        client: ApiClient,
        host: Weak<Host>,
        session_id: String,
        deliver: Arc<dyn Fn(Value) + Send + Sync>,
    ) -> Result<(Self, Value), String> {
        let (downlinks, connected) = Self::spawn(client, host, session_id, deliver);

        match timeout(CONNECT_TIMEOUT, connected).await {
            Ok(Ok(Ok(snapshot))) => Ok((downlinks, snapshot)),
            Ok(Ok(Err(message))) => Err(message),
            // The reader ending without an answer is the same outcome as one
            // that never gave it.
            Ok(Err(_)) | Err(_) => {
                Err("the harness streams did not become ready in time".to_string())
            }
        }
    }

    fn spawn(
        client: ApiClient,
        host: Weak<Host>,
        session_id: String,
        deliver: Arc<dyn Fn(Value) + Send + Sync>,
    ) -> (Self, oneshot::Receiver<Result<Value, String>>) {
        let (connected_tx, connected) = oneshot::channel();
        let gate = DeliveryGate(Arc::new(Mutex::new(Some(deliver))));

        let gated: Delivery = {
            let gate = gate.clone();

            Arc::new(move |frame| gate.deliver(frame))
        };

        let reader = nmt_runtime::handle().spawn(run_downlink(
            client,
            host,
            session_id,
            gated,
            connected_tx,
        ));

        (
            Self {
                reader: reader.abort_handle(),
                gate,
            },
            connected,
        )
    }
}

async fn run_downlink(
    client: ApiClient,
    host: Weak<Host>,
    session_id: String,
    deliver: Arc<dyn Fn(Value) + Send + Sync>,
    connected_tx: oneshot::Sender<Result<Value, String>>,
) {
    let mut connected_tx = Some(connected_tx);

    loop {
        let result = read_downlink(&client, &session_id, deliver.as_ref(), &mut connected_tx).await;

        if let Err(message) = result {
            if let Some(sender) = connected_tx.take() {
                let _ = sender.send(Err(message));

                return;
            }

            warn!("deepseek stream disconnected: {message}");
        }

        // Every later call on this session targets a port nobody serves, so
        // the tab must learn the host is gone; ending the reader quietly would
        // leave it looking ready while each action fails. A missing host is
        // reported the same way because no replacement can reach this session.
        let host = host.upgrade();

        if !host.as_ref().is_some_and(|host| host.is_running()) {
            let status = host.and_then(|host| host.exit_status());

            warn!(?status, "deepseek harness host exited");

            deliver(json!({ "payload": {
                "type": "nmt/host-exited", "sessionId": session_id,
            } }));

            return;
        }

        deliver(json!({ "payload": {
            "type": "nmt/connection-reset", "sessionId": session_id,
        } }));

        sleep(RECONNECT_DELAY).await;
    }
}

impl Drop for Downlinks {
    fn drop(&mut self) {
        self.reader.abort();

        // A frame delivered after this returns would reach a tab that has
        // already moved to another conversation, so delivery closes here.
        drop(self.gate.close());
    }
}

async fn connect_downlink(api: &ApiClient) -> Result<Socket, String> {
    let request = api.stream_request()?;

    match timeout(CONNECT_TIMEOUT, connect_async(request)).await {
        Ok(Ok((socket, _))) => Ok(socket),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("the harness stream did not connect in time".to_string()),
    }
}

async fn read_downlink(
    client: &ApiClient,
    session_id: &str,
    deliver: &(dyn Fn(Value) + Send + Sync),
    connected: &mut Option<oneshot::Sender<Result<Value, String>>>,
) -> Result<(), String> {
    let mut socket = connect_downlink(client).await?;

    for (id, endpoint, args) in [
        ("events", "$events", json!({})),
        ("control", "session/control", json!({})),
        (
            "follow",
            "session/follow",
            follow_args(session_address(session_id), 200),
        ),
    ] {
        open_stream(&mut socket, id, endpoint, args).await?;
    }

    let mut streams = Streams::new(session_id);

    let (failed_tx, mut failed) = unbounded_channel();

    loop {
        let message = tokio::select! {
            message = socket.next() => message,
            Some(message) = failed.recv() => return Err(message),
        };

        let Some(frame) = read_message(&mut socket, message).await? else {
            continue;
        };

        if let Some(pass) = streams.process(frame, deliver)? {
            pass_event(client.clone(), pass, failed_tx.clone());
        }

        if let Some(snapshot) = streams.ready_snapshot()
            && let Some(sender) = connected.take()
        {
            let _ = sender.send(Ok(snapshot.clone()));
        }
    }
}

/// Decline an interaction this tab does not present, without holding up the
/// reader: the host drops a socket that misses two of its two-second
/// heartbeats, and a reply is a call the host may take longer than that to
/// answer. A reply that fails is reported back so the connection is replaced,
/// which makes the host offer the still-pending interaction again.
fn pass_event(client: ApiClient, pass: PassedEvent, failed: UnboundedSender<String>) {
    nmt_runtime::handle().spawn(async move {
        let reply = client
            .respond_event(&pass.client_id, &pass.event_id, json!({ "kind": "next" }))
            .await;

        if let Err(error) = reply {
            let _ = failed.send(error.message().to_string());
        }
    });
}

pub(crate) fn session_address(session_id: &str) -> Value {
    json!({ "kind": "session", "sessionId": session_id })
}

/// Live tokens travel only on the opted-in assistant stream: the durable log
/// records a reply once its step commits, so without the flag a reply appears
/// whole at the end of each step instead of word by word.
fn follow_args(address: Value, max_messages: u64) -> Value {
    json!({ "request": {
        "address": address, "maxMessages": max_messages, "assistantStream": true,
    } })
}

async fn open_stream(
    socket: &mut Socket,
    id: &str,
    endpoint: &str,
    args: Value,
) -> Result<(), String> {
    socket
        .send(Message::Text(
            json!({
                "type": "open", "streamId": id, "endpoint": endpoint,
                "payload": { "args": args },
            })
            .to_string()
            .into(),
        ))
        .await
        .map_err(|error| error.to_string())
}

/// The follow opening is a consistent, cold-readable history window with its
/// projection cursor. Closing immediately after it avoids maintaining another
/// subscription for child transcripts or branch-point pickers.
pub(crate) async fn snapshot(
    client: &ApiClient,
    address: Value,
    max_messages: u64,
) -> Result<Value, CallError> {
    let read = async {
        let mut socket = connect_downlink(client).await?;

        open_stream(
            &mut socket,
            "snapshot",
            "session/follow",
            follow_args(address, max_messages),
        )
        .await?;

        loop {
            let message = socket.next().await;

            match read_message(&mut socket, message).await? {
                Some(frame) if frame["type"] == "item" && frame["value"]["type"] == "snapshot" => {
                    let _ = socket.close(None).await;

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
    };

    match timeout(CONNECT_TIMEOUT, read).await {
        Ok(page) => page,
        Err(_) => Err("the harness history snapshot did not arrive in time".to_string()),
    }
    .map_err(CallError::Transport)
}

async fn read_message(
    socket: &mut Socket,
    message: Option<Result<Message, Error>>,
) -> Result<Option<Value>, String> {
    match message {
        Some(Ok(Message::Text(text))) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| error.to_string()),
        Some(Ok(Message::Close(_))) | None => Err("the harness closed the stream".to_string()),
        // Reading queues the Ping reply; flushing sends it now rather than
        // with the next outgoing message, which an idle stream never has.
        Some(Ok(Message::Ping(_))) => socket
            .flush()
            .await
            .map(|_| None)
            .map_err(|error| error.to_string()),
        Some(Ok(_)) => Ok(None),
        Some(Err(error)) => Err(error.to_string()),
    }
}

// Decode logical streams into the adapter's local delivery messages.

/// An interaction the stream offered that this tab leaves to other clients.
#[derive(Debug, PartialEq, Eq)]
struct PassedEvent {
    client_id: String,
    event_id: String,
}

struct Streams {
    session_id: String,
    client_id: Option<String>,
    control_ready: bool,
    snapshot: Option<Value>,
    pending: HashMap<String, &'static str>,
    attempt: Option<LiveAttempt>,
}

/// The provider attempt whose tokens the assistant stream is carrying. Chunk
/// frames name only the attempt, while the transcript keys rows by turn and
/// step, which the start frame (or the reconnect baseline) announced once.
struct LiveAttempt {
    id: String,
    turn: u64,
    step: u64,
}

impl LiveAttempt {
    fn from_frame(frame: &Value) -> Option<Self> {
        Some(Self {
            id: frame["attemptId"].as_str()?.to_string(),
            turn: frame["turn"].as_u64()?,
            step: frame["step"].as_u64()?,
        })
    }

    fn chunk_event(&self, time: &Value, chunk: &Value) -> Value {
        json!({
            "type": "assistant/chunk", "time": time,
            "data": { "turn": self.turn, "step": self.step, "chunk": chunk },
        })
    }

    /// The compact stream a reconnect baseline carries packs consecutive
    /// deltas of one block into a texts array; the row shape the history
    /// replay already decodes.
    fn baseline_event(&self, record: &Value) -> Option<Value> {
        let kind = match record["type"].as_str()? {
            "chunk" => return Some(self.chunk_event(&record["time"], &record["chunk"])),
            "text-chunks" => "chunkrow/text-chunks",
            "reasoning-chunks" => "chunkrow/reasoning-chunks",
            _ => return None,
        };

        Some(json!({
            "type": kind, "time": record["time0"],
            "data": {
                "turn": self.turn, "step": self.step,
                "index": record["index"], "texts": record["texts"],
            },
        }))
    }
}

impl Streams {
    fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            client_id: None,
            control_ready: false,
            snapshot: None,
            pending: HashMap::new(),
            attempt: None,
        }
    }

    fn ready_snapshot(&self) -> Option<&Value> {
        if self.client_id.is_some() && self.control_ready {
            self.snapshot.as_ref()
        } else {
            None
        }
    }

    /// Deliver what one stream message means to the tab, and name the
    /// interaction it offered when that one is for other clients to answer.
    fn process(
        &mut self,
        frame: Value,
        deliver: &dyn Fn(Value),
    ) -> Result<Option<PassedEvent>, String> {
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
            "events" => return self.on_event(value, deliver),
            "control" => self.on_control(value, deliver),
            "follow" => self.on_follow(value, deliver),
            _ => {}
        }

        Ok(None)
    }

    fn on_follow(&mut self, value: &Value, deliver: &dyn Fn(Value)) {
        match value["type"].as_str() {
            Some("snapshot") => {
                deliver(json!({ "payload": {
                                "type": "nmt/replay", "sessionId": self.session_id, "page": value,
                            } }));

                self.snapshot = Some(value.clone());

                // A reply already under way when the follow opened is absent
                // from the durable page; its prefix arrives as a baseline.
                let opening = &value["assistantStream"]["activeAttempt"];

                self.attempt = LiveAttempt::from_frame(opening);

                if let Some(attempt) = &self.attempt {
                    for record in opening["stream"].as_array().into_iter().flatten() {
                        if let Some(event) = attempt.baseline_event(record) {
                            self.event(event, deliver);
                        }
                    }
                }
            }
            Some("event") => self.event(value["event"].clone(), deliver),
            Some("assistant-stream") => self.on_assistant_stream(&value["frame"], deliver),
            _ => {}
        }
    }

    fn on_assistant_stream(&mut self, frame: &Value, deliver: &dyn Fn(Value)) {
        match frame["type"].as_str() {
            Some("start") => self.attempt = LiveAttempt::from_frame(frame),
            Some("chunk") => {
                if let Some(attempt) = &self.attempt
                    && frame["attemptId"] == attempt.id.as_str()
                {
                    self.event(
                        attempt.chunk_event(&frame["time"], &frame["chunk"]),
                        deliver,
                    );
                }
            }
            Some("end") => self.attempt = None,
            _ => {}
        }
    }

    fn event(&self, event: Value, deliver: &dyn Fn(Value)) {
        deliver(json!({ "payload": {
            "type": "session/event", "sessionId": self.session_id, "event": event,
        } }));
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

                // A host that keeps no job registry sends no jobs map, and an
                // empty list from here would read as every job having ended.
                // With the map present, a missing entry means none are held.
                if baseline["jobs"].is_object() {
                    self.jobs(&baseline["jobs"][&self.session_id], deliver);
                }

                self.control_ready = true;
            }
            Some("queue") if value["sessionId"] == self.session_id => {
                self.queue(&value["items"], deliver)
            }
            Some("jobs") if value["sessionId"] == self.session_id => {
                self.jobs(&value["jobs"], deliver)
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

    fn jobs(&self, jobs: &Value, deliver: &dyn Fn(Value)) {
        deliver(json!({ "payload": {
            "type": "session/jobs", "sessionId": self.session_id, "jobs": jobs,
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
        deliver: &dyn Fn(Value),
    ) -> Result<Option<PassedEvent>, String> {
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
            // The harness's own client takes a session's running state from
            // these edges rather than from its log, which can leave a turn
            // open when the turn-end record is rejected.
            Some("emit")
                if value["event"] == "api-session/status"
                    && value["args"][0] == self.session_id =>
            {
                deliver(json!({ "payload": {
                    "type": SESSION_STATUS, "sessionId": self.session_id, "running": value["args"][1],
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

                let Some((requested, resolved)) =
                    kind.filter(|_| value["agentId"] == self.session_id)
                else {
                    return Ok(Some(PassedEvent {
                        client_id: client_id.to_string(),
                        event_id: event_id.to_string(),
                    }));
                };

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

        Ok(None)
    }
}
