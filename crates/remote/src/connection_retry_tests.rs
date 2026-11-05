use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt as _;
use nmt_remote_core::identity::DeviceKey;
use nmt_remote_core::messages::RelayAccess;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use crate::client::PathPolicy;
use crate::connection::{RemoteHost, Retry, Status};
use crate::store::{PairedHost, StoredRelay};

const WAIT: Duration = Duration::from_secs(5);

async fn retries_after_final_attempt(notify: impl FnOnce(&Arc<RemoteHost>, &watch::Sender<u64>)) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let key = DeviceKey::generate().unwrap();

    let mut record = PairedHost::new(*key.public(), "Test host".into(), None);

    record.relay = Some(
        StoredRelay::seal(&RelayAccess {
            url: format!("ws://{}", listener.local_addr().unwrap()),
            access_key: "test".into(),
        })
        .unwrap(),
    );

    let host = RemoteHost::unstarted(
        record,
        Arc::new(DeviceKey::generate().unwrap()),
        "test".into(),
        Retry::Limited {
            attempts: 1,
            window: WAIT,
        },
        |_| {},
    );

    host.set_path_policy(PathPolicy::Relay);
    host.keep_connected();

    let (changes, network) = watch::channel(0);

    let (accepted, mut attempts) = mpsc::unbounded_channel();

    let server = tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let socket = accept_async(tcp).await.unwrap();

            if accepted.send(socket).is_err() {
                return;
            }
        }
    });

    let supervisor = tokio::spawn(Arc::clone(&host).supervise(network));

    let mut first = timeout(WAIT, attempts.recv()).await.unwrap().unwrap();

    notify(&host, &changes);

    first
        .close(Some(CloseFrame {
            code: CloseCode::Error,
            reason: "temporary relay failure".into(),
        }))
        .await
        .unwrap();

    let next = timeout(Duration::from_secs(1), attempts.recv()).await;

    supervisor.abort();
    server.abort();

    assert!(
        matches!(next, Ok(Some(_))),
        "a notification during the final attempt did not start another round"
    );
}

#[tokio::test]
async fn foreground_resume_during_the_final_attempt_retries() {
    retries_after_final_attempt(|host, _| {
        host.stop_listing();
        host.keep_connected();
    })
    .await;
}

#[tokio::test]
async fn network_change_during_the_final_attempt_retries() {
    retries_after_final_attempt(|_, changes| changes.send_modify(|version| *version += 1)).await;
}

#[tokio::test]
async fn a_request_during_the_final_attempt_retries_without_an_open_view() {
    retries_after_final_attempt(|host, _| {
        host.stop_listing();

        assert!(host.list_sessions().now_or_never().is_none());
    })
    .await;
}

#[tokio::test]
async fn the_initial_request_does_not_start_another_retry_round() {
    let key = DeviceKey::generate().unwrap();
    let record = PairedHost::new(*key.public(), "Test host".into(), None);

    let host = RemoteHost::unstarted(
        record,
        Arc::new(key),
        "test".into(),
        Retry::Limited {
            attempts: 1,
            window: WAIT,
        },
        |_| {},
    );

    host.set_path_policy(PathPolicy::Relay);
    host.keep_connected();

    let (_changes, network) = watch::channel(0);

    let mut status = host.status();

    let supervisor = tokio::spawn(Arc::clone(&host).supervise(network));

    timeout(
        WAIT,
        status.wait_for(|current| *current == Status::Unreachable),
    )
    .await
    .unwrap()
    .unwrap();

    assert!(
        timeout(Duration::from_millis(100), status.changed())
            .await
            .is_err()
    );

    supervisor.abort();
}
