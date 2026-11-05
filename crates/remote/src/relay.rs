//! The user's own relay: a Cloudflare Worker (under `relay/`) that pairs a
//! host's sockets with its clients' and forwards their frames without
//! reading them. Every channel through it is the same end-to-end Noise
//! channel as on the LAN, so the relay can cost availability, never
//! confidentiality. Its access key only guards the owner's quota.

use anyhow::{Context as _, Error, Result, anyhow};
use futures::StreamExt as _;
use nmt_net::connect_websocket;
use nmt_remote_core::identity::DeviceId;
use nmt_remote_core::messages::RelayAccess;
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::{Span, info};

use crate::link::ReceiveEnd;
use crate::{CONNECT_TIMEOUT, connection_stage};

pub(crate) type RelaySocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum RelayMessage {
    Open { id: Option<String> },
    Pong,
}

/// Connect to `path` on the relay with the access key and optional host
/// token, through the proxy in the settings. `https://` and `http://` URLs
/// are accepted for their WebSocket equivalents.
pub(crate) async fn open(
    relay: &RelayAccess,
    path: &str,
    host_token: Option<&str>,
) -> Result<RelaySocket> {
    let base = relay.url.trim_end_matches('/');

    let base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_owned()
    };

    let mut request = format!("{base}{path}").into_client_request()?;

    let headers = request.headers_mut();

    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {}", relay.access_key))?,
    );

    if let Some(token) = host_token {
        headers.insert("X-Host-Token", HeaderValue::from_str(token)?);
    }

    let (ws, response) = connection_stage::run("relay_websocket", connect_websocket(request))
        .await
        .with_context(|| format!("reaching the relay at {}", relay.url))?;

    if let Some(id) = response
        .headers()
        .get("X-Relay-Connection")
        .and_then(|value| value.to_str().ok())
    {
        Span::current().record("relay_connection", id);

        info!(relay_connection = id, "relay WebSocket connected");
    }

    Ok(ws)
}

/// Join a host's room as a client and wait until the host picks up. The
/// relay forwards nothing before that, so the handshake starts afterwards.
pub(crate) async fn dial_host(relay: &RelayAccess, host: &DeviceId) -> Result<RelaySocket> {
    let ws = open(relay, &format!("/v1/client/{}", host.as_str()), None).await?;

    connection_stage::run("relay_pickup", wait_for_open(ws)).await
}

/// Join, through the relay, the host showing a pairing code with `slot`.
pub(crate) async fn dial_pairing(relay: &RelayAccess, slot: &str) -> Result<RelaySocket> {
    let ws = open(relay, &format!("/v1/pair/{slot}"), None).await?;

    connection_stage::run("relay_pickup", wait_for_open(ws)).await
}

async fn wait_for_open(mut ws: RelaySocket) -> Result<RelaySocket> {
    timeout(CONNECT_TIMEOUT, async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<RelayMessage>(&text)?
                {
                    RelayMessage::Open { id } => {
                        if let Some(id) = id {
                            Span::current().record("relay_connection", &id);

                            info!(relay_connection = %id, "relay host accepted the connection");
                        }

                        return Ok(());
                    }
                    RelayMessage::Pong => {}
                },
                Some(Ok(Message::Close(frame))) => {
                    let offline = frame
                        .as_ref()
                        .is_some_and(|frame| frame.code == CloseCode::from(4404));

                    if offline {
                        return Err(Error::from(ReceiveEnd::Close(frame))
                            .context("the host is not connected to the relay"));
                    }

                    return Err(ReceiveEnd::Close(frame).into());
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(error.into()),
                None => return Err(ReceiveEnd::StreamEnd.into()),
            }
        }
    })
    .await
    .map_err(|_| anyhow!("the host did not answer through the relay"))??;

    Ok(ws)
}
