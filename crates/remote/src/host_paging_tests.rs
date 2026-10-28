use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nmt_remote_core::channel::{Channel, ClientHandshake, HostHandshake};
use nmt_remote_core::frame::{CONTROL_STREAM, Message as FrameMessage, kind};
use nmt_remote_core::identity::DeviceKey;
use nmt_remote_core::messages::{ClientHello, DeviceInfo, DeviceKind, HostHello};
use nmt_remote_core::preface::{Preface, PrefaceKind};
use nmt_remote_core::rpc::{self, AgentAttached, AgentOps, Control, ErrorCode, RpcError};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::io::duplex;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::Role;

use crate::host::{HostConfig, Shared, State, run_channel};
use crate::link::{ControlChunks, MAX_CONTROL_TRANSFER, Outbound, SendQueue, pump};
use crate::sessions::{AgentControl, AgentRequest, SessionRegistry};

const WAIT: Duration = Duration::from_secs(30);

fn channels(client: &DeviceKey, host: &DeviceKey) -> (Channel, Channel) {
    let offer = Preface::offer(PrefaceKind::Channel);
    let answer = offer.answer();

    let (pending, request) = ClientHandshake::start(
        client,
        host.public(),
        &offer,
        &answer,
        &ClientHello {
            proto_minor: 1,
            app_version: String::new(),
            features: Vec::new(),
            hello_ms: 1,
        },
    )
    .unwrap();

    let (host_channel, response) = HostHandshake::read(host, &offer, &answer, &request)
        .unwrap()
        .accept(&HostHello {
            proto_minor: 1,
            app_version: String::new(),
            features: Vec::new(),
            name: String::new(),
            lan_hints: Vec::new(),
        })
        .unwrap();

    let (client_channel, _) = pending.finish(&response).unwrap();

    (client_channel, host_channel)
}

fn shared(registry: Arc<SessionRegistry>) -> Arc<Shared> {
    Arc::new(Shared {
        key: DeviceKey::generate().unwrap(),
        dir: PathBuf::new(),
        config: HostConfig {
            port: 0,
            device: DeviceInfo {
                name: "Memory host".into(),
                kind: DeviceKind::Desktop,
                platform: "test".into(),
                app_version: "0.0.0".into(),
            },
            shell: None,
            args: Vec::new(),
            registry,
            relay: None,
            announce: false,
            on_change: Arc::new(|| {}),
            stun_servers: Vec::new(),
        },
        name: watch::channel("Memory host".to_owned()).0,
        state: Mutex::new(State::default()),
        unauthenticated: Arc::new(Semaphore::new(1)),
        advertiser: Mutex::new(None),
        relay: None,
        relay_slot: Mutex::new(None),
        push_client: reqwest::Client::new(),
        lan_port: 0,
        lan_paused: Default::default(),
    })
}

/// The actual host request loop and encrypted pumps over memory streams.
struct MemoryClient {
    queue: SendQueue,
    received: UnboundedReceiver<FrameMessage>,
    chunks: ControlChunks,
    pending: VecDeque<Control>,
    host: JoinHandle<anyhow::Result<()>>,
    client: JoinHandle<anyhow::Result<()>>,
}

impl MemoryClient {
    async fn start(shared: Arc<Shared>) -> Self {
        let key = DeviceKey::generate().unwrap();
        let (client_channel, host_channel) = channels(&key, &shared.key);
        let (client_io, host_io) = duplex(1 << 20);
        let client_ws = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let host_ws = WebSocketStream::from_raw_socket(host_io, Role::Server, None).await;
        let host = tokio::spawn(run_channel(shared, host_ws, host_channel, *key.public()));
        let (queue, outgoing) = SendQueue::new();
        let (incoming, received) = mpsc::unbounded_channel();

        let client = tokio::spawn(pump(
            client_ws,
            client_channel,
            outgoing,
            incoming,
            Arc::default(),
        ));

        Self {
            queue,
            received,
            chunks: ControlChunks::default(),
            pending: VecDeque::new(),
            host,
            client,
        }
    }

    async fn receive(&mut self) -> Control {
        timeout(WAIT, async {
            loop {
                let message = self
                    .received
                    .recv()
                    .await
                    .expect("host channel remains open");

                let bytes = match (message.stream, message.kind) {
                    (CONTROL_STREAM, kind::CONTROL_JSON) => message.payload,
                    (CONTROL_STREAM, kind::CONTROL_CHUNK) => {
                        let Some(bytes) = self.chunks.push(&message.payload).unwrap() else {
                            continue;
                        };

                        bytes
                    }
                    _ => panic!("unexpected frame"),
                };

                return Control::decode(&bytes).unwrap();
            }
        })
        .await
        .expect("host replies over the memory transport")
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) -> Result<Value, RpcError> {
        assert!(
            self.queue.send(Outbound::new(
                CONTROL_STREAM,
                kind::CONTROL_JSON,
                Control::Request {
                    id,
                    method: method.to_owned(),
                    params
                }
                .encode(),
            ))
        );

        loop {
            match self.receive().await {
                Control::Response {
                    id: response,
                    outcome,
                } if response == id => return outcome,
                event => self.pending.push_back(event),
            }
        }
    }

    async fn event(&mut self) -> Value {
        let event = match self.pending.pop_front() {
            Some(event) => event,
            None => self.receive().await,
        };

        match event {
            Control::Notification { method, params } => {
                assert_eq!(method, rpc::AGENT_PAGED_OPS);

                serde_json::from_value::<AgentOps>(params).unwrap().ops
            }
            _ => panic!("expected a paged notification"),
        }
    }

    async fn list_sessions(&mut self, id: u64) {
        let list = self
            .request(id, rpc::SESSIONS_LIST, json!({}))
            .await
            .unwrap();

        assert!(
            list["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["session"] == "memory-agent")
        );
        assert!(!self.host.is_finished());
        assert!(!self.client.is_finished());
    }
}

impl Drop for MemoryClient {
    fn drop(&mut self) {
        self.client.abort();
        self.host.abort();
    }
}

/// Opaque synthetic agent data allows host forwarding tests without agent dependencies.
fn agent(registry: &SessionRegistry, snapshot_bytes: usize, update_bytes: usize) {
    let (requests, mut incoming) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        let mut views = HashMap::new();

        while let Some(request) = incoming.recv().await {
            match request {
                AgentRequest::AttachPaged {
                    view_id,
                    updates,
                    reply,
                } => {
                    let _ = reply.send(json!({
                        "view_id": view_id, "type": "snapshot",
                        "value": { "body": "x".repeat(snapshot_bytes) },
                    }));

                    let _ = updates
                        .send(json!({
                            "view_id": view_id, "type": "update",
                            "value": { "sequence": 1, "body": "y".repeat(update_bytes) },
                        }))
                        .await;

                    views.insert(view_id, updates);
                }
                AgentRequest::Page {
                    view_id,
                    cursor,
                    reply,
                } => {
                    let result = if let Some(updates) = views.get(&view_id) {
                        updates.send(json!({
                            "view_id": view_id, "type": "page",
                            "value": { "requested": cursor, "entries": ["older synthetic entry"] },
                        })).await.map(|()| json!({})).map_err(|_| "view closed".into())
                    } else {
                        Err("view missing".into())
                    };

                    let _ = reply.send(result);
                }
                AgentRequest::Attach { reply, .. } => drop(reply),
                AgentRequest::Call { reply, .. } => {
                    let _ = reply.send(Ok(json!({})));
                }
            }
        }
    });

    registry.register_agent(
        "memory-agent".into(),
        "Synthetic".into(),
        "Test".into(),
        AgentControl { requests },
    );
}

async fn snapshot(client: &mut MemoryClient, id: u64, view_id: &str) -> Result<Value, RpcError> {
    let reply = client
        .request(
            id,
            rpc::AGENT_ATTACH_PAGED,
            json!({
                "session": "memory-agent", "view_id": view_id,
            }),
        )
        .await?;

    let attached = serde_json::from_value::<AgentAttached>(reply).unwrap();

    assert_eq!(attached.view["type"], "snapshot");
    assert_eq!(attached.view["view_id"], view_id);

    Ok(attached.view["value"].clone())
}

#[tokio::test]
async fn memory_paged_large_snapshot_update_page_and_stale_view() {
    let registry = SessionRegistry::new();
    let bytes = 9 * 1024 * 1024;

    agent(&registry, bytes, bytes);

    let shared = shared(registry);

    let mut client = MemoryClient::start(Arc::clone(&shared)).await;

    let first = snapshot(&mut client, 1, "first-memory-view").await.unwrap();

    assert_eq!(first["body"].as_str().unwrap().len(), bytes);

    drop(first);

    let update = client.event().await;

    assert_eq!(update["type"], "update");
    assert_eq!(update["value"]["body"].as_str().unwrap().len(), bytes);

    drop(update);

    let cursor = json!({ "start": 20, "revision": 4 });

    client
        .request(
            2,
            rpc::AGENT_PAGE,
            json!({
                "session": "memory-agent", "view_id": "first-memory-view", "cursor": cursor,
            }),
        )
        .await
        .unwrap();

    let page = client.event().await;

    assert_eq!(page["type"], "page");
    assert_eq!(page["value"]["requested"], cursor);

    let error = client
        .request(
            3,
            rpc::AGENT_PAGE,
            json!({
                "session": "memory-agent", "view_id": "stale-memory-view", "cursor": {},
            }),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::Denied);

    client
        .request(4, rpc::AGENT_DETACH, json!({ "session": "memory-agent" }))
        .await
        .unwrap();

    let error = client
        .request(
            5,
            rpc::AGENT_PAGE,
            json!({
                "session": "memory-agent", "view_id": "first-memory-view", "cursor": {},
            }),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::Denied);

    snapshot(&mut client, 6, "reopened-memory-view")
        .await
        .unwrap();

    assert_eq!(client.event().await["view_id"], "reopened-memory-view");

    client.list_sessions(7).await;

    drop(client);

    let mut reconnected = MemoryClient::start(shared).await;

    let error = reconnected
        .request(
            8,
            rpc::AGENT_PAGE,
            json!({
                "session": "memory-agent", "view_id": "reopened-memory-view", "cursor": {},
            }),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::Denied);

    snapshot(&mut reconnected, 9, "reconnected-memory-view")
        .await
        .unwrap();

    assert_eq!(
        reconnected.event().await["view_id"],
        "reconnected-memory-view"
    );

    reconnected.list_sessions(10).await;
}

#[tokio::test]
async fn memory_paged_oversized_snapshot_returns_error_and_keeps_channel_open() {
    let registry = SessionRegistry::new();

    agent(&registry, MAX_CONTROL_TRANSFER, 0);

    let mut client = MemoryClient::start(shared(registry)).await;

    let error = snapshot(&mut client, 1, "oversized-snapshot")
        .await
        .unwrap_err();

    assert_eq!(error.code, ErrorCode::InvalidParams);
    assert!(error.message.contains("64 MiB"));

    client.list_sessions(2).await;
}

#[tokio::test]
async fn memory_paged_oversized_update_returns_error_and_keeps_channel_open() {
    let registry = SessionRegistry::new();

    agent(&registry, 1, MAX_CONTROL_TRANSFER);

    let mut client = MemoryClient::start(shared(registry)).await;

    snapshot(&mut client, 1, "oversized-update").await.unwrap();

    let error = client.event().await;

    assert_eq!(error["type"], "error");
    assert!(
        error["value"]["message"]
            .as_str()
            .unwrap()
            .contains("64 MiB")
    );

    client.list_sessions(2).await;
}
