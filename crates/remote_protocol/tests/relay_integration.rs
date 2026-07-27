//! End-to-end relay tests: two in-process endpoints (host + client) talk
//! through the real Cloudflare Worker running locally.
//!
//! Requires `wrangler dev` listening on 127.0.0.1:8787 (run `npm run dev` in
//! `relay/`), so these are `#[ignore]` by default:
//!
//! ```text
//! cargo test -p nmt_remote_protocol --test relay_integration -- --ignored
//! ```

use futures::{SinkExt, StreamExt};
use nmt_remote_protocol::{Frame, Handshake, generate_keypair};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

const RELAY: &str = "ws://127.0.0.1:8787/ws";
const TOKEN: &str = "test-token";

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn connect(query: &str, token: Option<&str>) -> Result<Socket, String> {
    let mut request = format!("{RELAY}?{query}")
        .into_client_request()
        .expect("valid request");
    if let Some(token) = token {
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {token}").parse().expect("valid header"),
        );
    }
    match connect_async(request).await {
        Ok((socket, _)) => Ok(socket),
        Err(e) => Err(e.to_string()),
    }
}

async fn next_binary(socket: &mut Socket) -> Vec<u8> {
    loop {
        match socket.next().await.expect("socket open").expect("no error") {
            Message::Binary(data) => return data.to_vec(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("expected binary message, got {other:?}"),
        }
    }
}

async fn next_json(socket: &mut Socket) -> serde_json::Value {
    loop {
        match socket.next().await.expect("socket open").expect("no error") {
            Message::Text(text) => return serde_json::from_str(&text).expect("valid JSON"),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("expected text message, got {other:?}"),
        }
    }
}

#[tokio::test]
#[ignore = "requires `wrangler dev` running in relay/ (npm run dev)"]
async fn noise_echo_through_relay() {
    let host_keys = generate_keypair().unwrap();
    let client_keys = generate_keypair().unwrap();
    let host_id = nmt_remote_protocol::derive_host_id(&host_keys.public);

    // Host registers its control socket and receives the reconciliation sync.
    let mut control = connect(&format!("host_id={host_id}&role=host"), Some(TOKEN))
        .await
        .expect("host registration must succeed");
    let sync = next_json(&mut control).await;
    assert_eq!(sync["type"], "sync");

    // Client connects and immediately sends Noise message 1 — the relay must
    // buffer it because no host data socket exists yet.
    let mut client_sock = connect(&format!("host_id={host_id}&role=client"), None)
        .await
        .expect("client connect must succeed");
    let mut client_hs = Handshake::initiator_ik(&client_keys.private, &host_keys.public).unwrap();
    let msg1 = client_hs.write_message().unwrap();
    client_sock
        .send(Message::Binary(msg1.into()))
        .await
        .unwrap();

    // Relay tells the host about the new client; host dials the data socket.
    let connected = next_json(&mut control).await;
    assert_eq!(connected["type"], "connected");
    let cid = connected["connectionId"].as_str().unwrap().to_owned();
    assert!(cid.starts_with("conn_"), "relay-assigned id, got {cid}");

    let mut host_data = connect(
        &format!("host_id={host_id}&role=host&connection_id={cid}"),
        Some(TOKEN),
    )
    .await
    .expect("host data socket must succeed");

    // The buffered message 1 arrives; host verifies the device and replies.
    let mut host_hs = Handshake::responder_ik(&host_keys.private).unwrap();
    host_hs
        .read_message(&next_binary(&mut host_data).await)
        .unwrap();
    assert_eq!(
        host_hs.remote_static(),
        Some(client_keys.public.as_slice()),
        "authorized-device check happens on this key"
    );
    let msg2 = host_hs.write_message().unwrap();
    host_data.send(Message::Binary(msg2.into())).await.unwrap();

    client_hs
        .read_message(&next_binary(&mut client_sock).await)
        .unwrap();
    let mut client_chan = client_hs.into_transport().unwrap();
    let mut host_chan = host_hs.into_transport().unwrap();

    // Encrypted echo round trip, client → host → client.
    let ping = Frame::Input {
        session_id: 1,
        data: b"echo hello".to_vec(),
    };
    let ct = client_chan.seal(&ping.encode().unwrap()).unwrap();
    client_sock.send(Message::Binary(ct.into())).await.unwrap();
    let received = Frame::decode(&host_chan.open(&next_binary(&mut host_data).await).unwrap());
    assert_eq!(received.unwrap(), ping);

    let pong = Frame::Output {
        session_id: 1,
        seq: 0,
        data: b"hello".to_vec(),
    };
    let ct = host_chan.seal(&pong.encode().unwrap()).unwrap();
    host_data.send(Message::Binary(ct.into())).await.unwrap();
    let received = Frame::decode(
        &client_chan
            .open(&next_binary(&mut client_sock).await)
            .unwrap(),
    );
    assert_eq!(received.unwrap(), pong);
}

#[tokio::test]
#[ignore = "requires `wrangler dev` running in relay/ (npm run dev)"]
async fn invalid_token_rejected() {
    let err = connect("host_id=deadbeef00000000&role=host", Some("wrong-token"))
        .await
        .expect_err("wrong token must be rejected");
    assert!(err.contains("401"), "expected HTTP 401, got: {err}");

    let err = connect("host_id=deadbeef00000000&role=host", None)
        .await
        .expect_err("missing token must be rejected");
    assert!(err.contains("401"), "expected HTTP 401, got: {err}");
}

#[tokio::test]
#[ignore = "requires `wrangler dev` running in relay/ (npm run dev)"]
async fn client_rejected_when_host_offline() {
    let err = connect("host_id=0000000000000000&role=client", None)
        .await
        .expect_err("client must be rejected when host is offline");
    assert!(err.contains("404"), "expected HTTP 404, got: {err}");
}

#[tokio::test]
#[ignore = "requires `wrangler dev` running in relay/ (npm run dev)"]
async fn buffer_overflow_closes_client() {
    let host_keys = generate_keypair().unwrap();
    let host_id = nmt_remote_protocol::derive_host_id(&host_keys.public);

    let mut control = connect(&format!("host_id={host_id}&role=host"), Some(TOKEN))
        .await
        .expect("host registration must succeed");
    let _sync = next_json(&mut control).await;

    // Client floods frames while the host never opens a data socket: the
    // relay's 200-frame buffer cap must close the client, not grow unbounded.
    let mut client_sock = connect(&format!("host_id={host_id}&role=client"), None)
        .await
        .expect("client connect must succeed");
    for _ in 0..201 {
        client_sock
            .send(Message::Binary(vec![0u8; 64].into()))
            .await
            .expect("send while relay still accepts");
    }
    loop {
        match client_sock.next().await {
            Some(Ok(Message::Close(Some(frame)))) => {
                assert_eq!(
                    frame.code,
                    CloseCode::Library(4429),
                    "expected overflow close code"
                );
                break;
            }
            Some(Ok(_)) => continue,
            // Depending on timing the close can surface as a protocol error
            // after the relay drops us; that still proves the disconnect.
            Some(Err(_)) | None => break,
        }
    }
}
