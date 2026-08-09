use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::net::TcpStream;
use tokio::time;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::protocol::{
    CONNECT_MODE_IK, CONNECT_MODE_PAIR, ClientBound, Frame, FrameError, Handshake, HostBound,
    NoiseError, PairingCode, SecureChannel, StaticKeypair,
};

pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// JSON notifications the relay pushes on the host control socket.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelayControlMessage {
    Sync {
        connections: Vec<String>,
    },
    Connected {
        #[serde(rename = "connectionId")]
        connection_id: String,
    },
    Disconnected {
        #[serde(rename = "connectionId")]
        connection_id: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("websocket failure: {0}")]
    Ws(#[from] WsError),
    #[error("{0}")]
    Noise(#[from] NoiseError),
    #[error("{0}")]
    Frame(#[from] FrameError),
    #[error("peer closed the connection")]
    Closed,
    /// The peer (host or relay) violated the protocol or rejected us. Callers
    /// treat this as permanent: the peer made a decision (bad handshake,
    /// revoked device, killed session) that a retry cannot change.
    #[error("protocol violation: {0}")]
    Protocol(String),
    /// A failure local to this machine (runtime or thread construction,
    /// request building) that says nothing about the peer's state. Kept
    /// distinct from [`NetError::Protocol`] so reconnect logic keeps retrying:
    /// a transient local failure must not kill a resumable session.
    #[error("local failure: {0}")]
    Internal(String),
    #[error("timed out waiting for the remote peer")]
    Timeout,
}

impl NetError {
    pub(crate) fn permanent_reconnect_reason(&self) -> Option<&str> {
        match self {
            Self::Protocol(message) => Some(message),
            _ => None,
        }
    }
}

/// Every client-side network wait is bounded by this: a relay that accepts the
/// socket but never answers, or a host that is registered but wedged, would
/// otherwise park the caller forever — and the pairing path is driven straight
/// from a UI action.
pub const NET_TIMEOUT: Duration = Duration::from_secs(15);

/// Fail with [`NetError::Timeout`] instead of awaiting indefinitely.
pub async fn with_timeout<T>(
    future: impl Future<Output = Result<T, NetError>>,
) -> Result<T, NetError> {
    match time::timeout(NET_TIMEOUT, future).await {
        Ok(result) => result,
        Err(_) => Err(NetError::Timeout),
    }
}

pub fn relay_ws_url(relay_url: &str, host_id: &str, role: &str, cid: Option<&str>) -> String {
    let mut url = format!("{relay_url}?host_id={host_id}&role={role}");
    if let Some(cid) = cid {
        url.push_str("&connection_id=");
        url.push_str(cid);
    }
    url
}

pub async fn ws_connect(url: &str, bearer_token: Option<&str>) -> Result<WsStream, NetError> {
    // Request construction fails on malformed local input (URL, token), not on
    // anything the peer did, so these are Internal rather than Protocol.
    let mut request = url
        .into_client_request()
        .map_err(|e| NetError::Internal(e.to_string()))?;
    if let Some(token) = bearer_token {
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {token}")
                .parse()
                .map_err(|_| NetError::Internal("token contains invalid header bytes".into()))?,
        );
    }
    let (socket, _) = connect_async(request).await?;
    Ok(socket)
}

/// Read the next binary message, skipping ping/pong control chatter.
pub async fn next_binary(ws: &mut WsStream) -> Result<Vec<u8>, NetError> {
    loop {
        match ws.next().await {
            None => return Err(NetError::Closed),
            Some(Err(e)) => return Err(e.into()),
            Some(Ok(Message::Binary(data))) => return Ok(data.to_vec()),
            Some(Ok(Message::Close(_))) => return Err(NetError::Closed),
            Some(Ok(_)) => continue,
        }
    }
}

/// An established end-to-end encrypted frame pipe over one WebSocket.
/// Sequential send/recv; callers needing full-duplex pumping split the work
/// into a select loop that owns this struct.
pub struct FrameChannel {
    pub ws: WsStream,
    pub chan: SecureChannel,
}

impl FrameChannel {
    pub async fn send(&mut self, frame: &Frame) -> Result<(), NetError> {
        let ciphertext = self.chan.seal(&frame.encode()?)?;
        self.ws.send(Message::Binary(ciphertext.into())).await?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<Frame, NetError> {
        let ciphertext = next_binary(&mut self.ws).await?;
        Ok(Frame::decode(&self.chan.open(&ciphertext)?)?)
    }

    pub async fn send_control<T: serde::Serialize>(&mut self, msg: &T) -> Result<(), NetError> {
        self.send(&Frame::control(msg)?).await
    }

    /// Receive frames until a Control arrives, decoded as `T`. Used by the
    /// client for request/response exchanges where data frames may interleave.
    pub async fn recv_control<T: DeserializeOwned>(&mut self) -> Result<T, NetError> {
        loop {
            if let Frame::Control(payload) = self.recv().await? {
                return Ok(Frame::parse_control(&payload)?);
            }
        }
    }
}

/// Client side, normal connection: IK handshake as a paired device. The relay
/// assigns our connection id server-side; we never need to know it.
pub async fn client_connect_ik(
    relay_url: &str,
    host_id: &str,
    host_public_key: &[u8],
    device: &StaticKeypair,
) -> Result<FrameChannel, NetError> {
    with_timeout(connect_ik(relay_url, host_id, host_public_key, device)).await
}

async fn connect_ik(
    relay_url: &str,
    host_id: &str,
    host_public_key: &[u8],
    device: &StaticKeypair,
) -> Result<FrameChannel, NetError> {
    let url = relay_ws_url(relay_url, host_id, "client", None);
    let mut ws = ws_connect(&url, None).await?;

    let mut handshake = Handshake::initiator_ik(&device.private, host_public_key)?;
    let mut first = vec![CONNECT_MODE_IK];
    first.extend_from_slice(&handshake.write_message()?);
    ws.send(Message::Binary(first.into())).await?;
    handshake.read_message(&next_binary(&mut ws).await?)?;

    Ok(FrameChannel {
        ws,
        chan: handshake.into_transport()?,
    })
}

/// Client side, first contact: XX handshake, then redeem the pairing token
/// in-channel. Verifies the host's static key against the pairing code so a
/// malicious relay cannot substitute its own responder.
pub async fn client_connect_pair(
    code: &PairingCode,
    device: &StaticKeypair,
    device_name: &str,
) -> Result<FrameChannel, NetError> {
    with_timeout(connect_pair(code, device, device_name)).await
}

async fn connect_pair(
    code: &PairingCode,
    device: &StaticKeypair,
    device_name: &str,
) -> Result<FrameChannel, NetError> {
    let url = relay_ws_url(&code.relay_url, &code.host_id, "client", None);
    let mut ws = ws_connect(&url, None).await?;

    let mut handshake = Handshake::initiator_xx(&device.private)?;
    let mut first = vec![CONNECT_MODE_PAIR];
    first.extend_from_slice(&handshake.write_message()?);
    ws.send(Message::Binary(first.into())).await?;
    handshake.read_message(&next_binary(&mut ws).await?)?;
    let msg3 = handshake.write_message()?;
    ws.send(Message::Binary(msg3.into())).await?;

    // XX reveals the responder's static key in message 2: pin it against the
    // out-of-band pairing code before trusting the channel with the token.
    let remote = handshake
        .remote_static()
        .ok_or_else(|| NetError::Protocol("responder sent no static key".into()))?;
    if remote != code.host_public_key {
        return Err(NetError::Protocol(
            "relay peer is not the host from the pairing code".into(),
        ));
    }

    let mut channel = FrameChannel {
        ws,
        chan: handshake.into_transport()?,
    };
    channel
        .send_control(&HostBound::Pair {
            token: code.token,
            device_name: device_name.to_owned(),
        })
        .await?;
    match channel.recv_control::<ClientBound>().await? {
        ClientBound::Paired => Ok(channel),
        ClientBound::Error { message, .. } => Err(NetError::Protocol(message)),
        other => Err(NetError::Protocol(format!(
            "unexpected pairing reply: {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::NetError;

    #[test]
    fn only_remote_rejections_stop_reconnect_attempts() {
        let rejection = NetError::Protocol("session was killed".into());
        assert_eq!(
            rejection.permanent_reconnect_reason(),
            Some("session was killed")
        );
        assert_eq!(
            NetError::Internal("runtime unavailable".into()).permanent_reconnect_reason(),
            None
        );
        assert_eq!(NetError::Timeout.permanent_reconnect_reason(), None);
    }
}
