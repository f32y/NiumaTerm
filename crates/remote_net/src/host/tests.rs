use std::time::Duration;
use std::{env, fs, process};

use futures::StreamExt;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

use crate::{HostConfig, HostHandle};

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
