use std::sync::atomic::AtomicBool;
use std::sync::{Arc, mpsc};
use std::time::Duration;
use std::{env, fs, process};

use futures::StreamExt;
use nmt_remote_session_hub::SessionEvent;
use tokio::net::TcpListener;
use tokio::sync::mpsc::unbounded_channel;
use tokio::time::timeout;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

use crate::host::forward_events;
use crate::{HostConfig, HostHandle};

#[test]
fn a_detached_subscription_reports_stream_loss_after_queued_output() {
    let (sender, receiver) = mpsc::channel();
    let (events, mut forwarded) = unbounded_channel();

    sender
        .send(SessionEvent::Output {
            seq: 1,
            data: Arc::from(b"last".as_slice()),
        })
        .unwrap();

    drop(sender);
    forward_events(&receiver, 42, events, Arc::new(AtomicBool::new(false)));

    assert!(matches!(
        forwarded.try_recv(),
        Ok((42, Some(SessionEvent::Output { seq: 1, .. })))
    ));
    assert!(matches!(forwarded.try_recv(), Ok((42, None))));
}

#[tokio::test]
async fn shutdown_disconnects_an_idle_relay() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let data_dir = env::temp_dir().join(format!(
        "nmt-host-shutdown-{}-{}",
        process::id(),
        address.port()
    ));

    let host = HostHandle::start(HostConfig {
        relay_url: format!("ws://{address}/ws"),
        access_token: "test-token".into(),
        data_dir: data_dir.clone(),
    })
    .unwrap();

    let (stream, _) = timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();

    let mut socket = accept_async(stream).await.unwrap();

    host.shutdown();

    let disconnected = timeout(Duration::from_secs(2), async {
        while let Some(message) = socket.next().await {
            if matches!(message, Err(_) | Ok(Message::Close(_))) {
                break;
            }
        }
    })
    .await;

    drop(host);
    fs::remove_dir_all(data_dir).unwrap();

    assert!(disconnected.is_ok(), "shutdown must close an idle relay");
}
