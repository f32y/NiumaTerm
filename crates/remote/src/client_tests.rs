use anyhow::Error;
use futures::SinkExt as _;
use nmt_remote_core::identity::DeviceKey;
use nmt_remote_core::messages::ClientHello;
use nmt_remote_core::preface::Preface;
use tokio::io::{AsyncWriteExt as _, duplex};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::client::{Refused, channel_handshake, merge_lan_hints};
use crate::link::{AUTH_REFUSED_CODE, ReceiveEnd, recv_binary, send_binary};

async fn authentication_failure(close: Option<CloseFrame>) -> Error {
    let key = DeviceKey::generate().unwrap();
    let host = DeviceKey::generate().unwrap();
    let (client_io, host_io) = duplex(4096);
    let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;

    let mut server = WebSocketStream::from_raw_socket(host_io, Role::Server, None).await;

    let hello = ClientHello {
        proto_minor: 0,
        app_version: "test".into(),
        features: Vec::new(),
        hello_ms: 1,
    };

    let peer = async {
        let offer = Preface::decode(&recv_binary(&mut server).await.unwrap()).unwrap();

        send_binary(&mut server, offer.answer().encode().to_vec())
            .await
            .unwrap();

        recv_binary(&mut server).await.unwrap();

        match close {
            Some(frame) => server.send(Message::Close(Some(frame))).await.unwrap(),
            None => server.get_mut().shutdown().await.unwrap(),
        }
    };

    let (result, ()) = tokio::join!(channel_handshake(client, &key, host.public(), &hello), peer);

    result
        .err()
        .expect("the host did not send an authentication reply")
}

#[tokio::test]
async fn a_reset_during_authentication_keeps_the_transport_error() {
    let error = authentication_failure(None).await;

    assert!(!error.is::<Refused>());
    assert!(error.is::<WsError>(), "{error:#}");
}

#[tokio::test]
async fn an_unrelated_close_during_authentication_is_retryable() {
    let error = authentication_failure(Some(CloseFrame {
        code: CloseCode::Error,
        reason: "relay peer error".into(),
    }))
    .await;

    assert!(!error.is::<Refused>());

    let Some(ReceiveEnd::Close(Some(frame))) = error.downcast_ref::<ReceiveEnd>() else {
        panic!("close status was lost: {error:#}");
    };

    assert_eq!(frame.code, CloseCode::Error);
    assert_eq!(frame.reason, "relay peer error");
}

#[tokio::test]
async fn the_unpaired_close_code_preserves_the_refusal_and_close_details() {
    let error = authentication_failure(Some(CloseFrame {
        code: CloseCode::from(AUTH_REFUSED_CODE),
        reason: "device not paired".into(),
    }))
    .await;

    assert!(error.is::<Refused>());
    assert!(error.is::<ReceiveEnd>());
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn the_hosts_list_replaces_what_was_known_behind_the_address_that_worked() {
    let hints = merge_lan_hints(
        &strings(&["192.168.1.20:47470", "10.0.0.5:47470"]),
        Some("10.0.0.5:47470"),
        &strings(&["192.168.1.9:47470"]),
    );

    // The stale 192.168.1.9 is gone; the working address leads and is not
    // listed twice.
    assert_eq!(hints, strings(&["10.0.0.5:47470", "192.168.1.20:47470"]));
}

#[test]
fn a_host_that_sends_no_list_keeps_the_known_addresses() {
    let hints = merge_lan_hints(
        &[],
        Some("192.168.1.20:47470"),
        &strings(&["192.168.1.9:47470", "192.168.1.20:47470"]),
    );

    assert_eq!(hints, strings(&["192.168.1.20:47470", "192.168.1.9:47470"]));
}

#[test]
fn malformed_addresses_are_dropped_and_the_list_is_bounded() {
    let hints = merge_lan_hints(
        &strings(&[
            "not an address",
            "10.0.0.1:1",
            "10.0.0.2:1",
            "10.0.0.3:1",
            "10.0.0.4:1",
            "10.0.0.5:1",
        ]),
        None,
        &[],
    );

    assert_eq!(
        hints,
        strings(&["10.0.0.1:1", "10.0.0.2:1", "10.0.0.3:1", "10.0.0.4:1"])
    );
}
