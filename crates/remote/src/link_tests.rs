use std::sync::Arc;
use std::time::Duration;

use futures::stream::{empty, iter};
use nmt_remote_core::channel::{Channel, ClientHandshake, HostHandshake};
use nmt_remote_core::frame::{CONTROL_STREAM, kind};
use nmt_remote_core::identity::DeviceKey;
use nmt_remote_core::messages::{ClientHello, HostHello};
use nmt_remote_core::preface::{Preface, PrefaceKind};
use tokio::io::{DuplexStream, duplex};
use tokio::sync::{Notify, mpsc};
use tokio::task::yield_now;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::link::{
    ControlChunks, MAX_CONTROL_TRANSFER, MAX_QUEUED, Outbound, ReceiveEnd, SendQueue, pump,
    recv_binary,
};

#[tokio::test]
async fn stream_end_and_a_close_without_status_remain_distinct() {
    let mut ended = empty::<Result<Message, WsError>>();

    let error = recv_binary(&mut ended).await.unwrap_err();

    assert!(matches!(
        error.downcast_ref::<ReceiveEnd>(),
        Some(ReceiveEnd::StreamEnd)
    ));

    let mut closed = iter([Ok(Message::Close(None))]);

    let error = recv_binary(&mut closed).await.unwrap_err();

    assert!(matches!(
        error.downcast_ref::<ReceiveEnd>(),
        Some(ReceiveEnd::Close(None))
    ));
}

fn channels() -> (Channel, Channel) {
    let client = DeviceKey::generate().unwrap();
    let host = DeviceKey::generate().unwrap();
    let offer = Preface::offer(PrefaceKind::Channel);
    let answer = offer.answer();

    let hello = ClientHello {
        proto_minor: 0,
        app_version: String::new(),
        features: Vec::new(),
        hello_ms: 1,
    };

    let (handshake, msg1) =
        ClientHandshake::start(&client, host.public(), &offer, &answer, &hello).unwrap();

    let (host_channel, msg2) = HostHandshake::read(&host, &offer, &answer, &msg1)
        .unwrap()
        .accept(&HostHello {
            proto_minor: 0,
            app_version: String::new(),
            features: Vec::new(),
            name: String::new(),
            lan_hints: Vec::new(),
        })
        .unwrap();

    let (client_channel, _) = handshake.finish(&msg2).unwrap();

    (client_channel, host_channel)
}

async fn sockets() -> (WebSocketStream<DuplexStream>, WebSocketStream<DuplexStream>) {
    let (a, b) = duplex(1 << 20);

    (
        WebSocketStream::from_raw_socket(a, Role::Client, None).await,
        WebSocketStream::from_raw_socket(b, Role::Server, None).await,
    )
}

#[tokio::test(start_paused = true)]
async fn quiet_but_live_channels_stay_open() {
    let (client_channel, host_channel) = channels();
    let (client_ws, host_ws) = sockets().await;
    let (_client_out, client_out_rx) = SendQueue::new();
    let (_host_out, host_out_rx) = SendQueue::new();
    let (client_in, _client_in_rx) = mpsc::unbounded_channel();
    let (host_in, _host_in_rx) = mpsc::unbounded_channel();

    let client = tokio::spawn(pump(
        client_ws,
        client_channel,
        client_out_rx,
        client_in,
        Arc::default(),
    ));

    let host = tokio::spawn(pump(
        host_ws,
        host_channel,
        host_out_rx,
        host_in,
        Arc::default(),
    ));

    sleep(Duration::from_secs(300)).await;

    assert!(!client.is_finished(), "pings keep an idle channel open");
    assert!(!host.is_finished());
}

#[tokio::test(start_paused = true)]
async fn a_silent_peer_is_declared_dead() {
    let (client_channel, _) = channels();

    // The peer socket exists but nothing ever reads or answers on it.
    let (client_ws, _silent) = sockets().await;
    let (_out, out_rx) = SendQueue::new();
    let (inbound, _inbound_rx) = mpsc::unbounded_channel();

    let result = timeout(
        Duration::from_secs(120),
        pump(client_ws, client_channel, out_rx, inbound, Arc::default()),
    )
    .await
    .expect("the pump gives up on its own");

    assert!(result.is_err());
}

#[tokio::test(start_paused = true)]
async fn a_probe_finds_a_dead_link_within_seconds() {
    let (client_channel, _) = channels();
    let (client_ws, _silent) = sockets().await;
    let (_out, out_rx) = SendQueue::new();
    let (inbound, _inbound_rx) = mpsc::unbounded_channel();
    let probe = Arc::new(Notify::new());

    // Asked for before the pump runs, as a change arriving mid-send would
    // be: the stored permit still triggers the probe.
    probe.notify_one();

    let result = timeout(
        Duration::from_secs(10),
        pump(client_ws, client_channel, out_rx, inbound, probe),
    )
    .await
    .expect("one missed probe closes the link, well before the idle timer");

    assert!(result.is_err());
}

#[tokio::test(start_paused = true)]
async fn a_probe_keeps_a_live_link() {
    let (client_channel, host_channel) = channels();
    let (client_ws, host_ws) = sockets().await;
    let (_client_out, client_out_rx) = SendQueue::new();
    let (_host_out, host_out_rx) = SendQueue::new();
    let (client_in, _client_in_rx) = mpsc::unbounded_channel();
    let (host_in, _host_in_rx) = mpsc::unbounded_channel();
    let probe = Arc::new(Notify::new());

    let client = tokio::spawn(pump(
        client_ws,
        client_channel,
        client_out_rx,
        client_in,
        Arc::clone(&probe),
    ));

    let host = tokio::spawn(pump(
        host_ws,
        host_channel,
        host_out_rx,
        host_in,
        Arc::default(),
    ));

    for _ in 0..5 {
        probe.notify_one();

        sleep(Duration::from_secs(20)).await;
    }

    assert!(
        !client.is_finished(),
        "an answered probe leaves the link open"
    );
    assert!(!host.is_finished());
}

#[tokio::test]
async fn bounded_admission_waits_for_bytes_to_drain() {
    let (queue, mut receiver) = SendQueue::new();

    let half = MAX_QUEUED / 2;

    assert!(
        queue
            .send_bounded(Outbound::new(0, kind::CONTROL_JSON, vec![0; half]))
            .await
    );

    let next = queue.send_bounded(Outbound::new(0, kind::CONTROL_JSON, vec![1]));

    tokio::pin!(next);

    assert!(futures::poll!(next.as_mut()).is_pending());
    assert_eq!(queue.queued_bytes(), half);

    assert_eq!(receiver.try_recv().unwrap().payload.len(), half);
    assert!(timeout(Duration::from_secs(1), next).await.unwrap());
    assert_eq!(receiver.try_recv().unwrap().payload, vec![1]);
    assert_eq!(queue.queued_bytes(), 0);
}

#[tokio::test]
async fn closing_the_receiver_wakes_blocked_admission() {
    let (queue, receiver) = SendQueue::new();

    assert!(
        queue
            .send_bounded(Outbound::new(
                0,
                kind::CONTROL_JSON,
                vec![0; MAX_QUEUED / 2]
            ))
            .await
    );

    let next = queue.send_bounded(Outbound::new(0, kind::CONTROL_JSON, vec![1]));

    tokio::pin!(next);

    assert!(futures::poll!(next.as_mut()).is_pending());

    drop(receiver);

    assert!(!timeout(Duration::from_secs(1), next).await.unwrap());
}

#[tokio::test]
async fn cancelling_admission_does_not_reserve_bytes() {
    let (queue, mut receiver) = SendQueue::new();

    assert!(
        queue
            .send_bounded(Outbound::new(
                0,
                kind::CONTROL_JSON,
                vec![0; MAX_QUEUED / 2]
            ))
            .await
    );

    {
        let next = queue.send_bounded(Outbound::new(0, kind::CONTROL_JSON, vec![1; 256]));

        tokio::pin!(next);

        assert!(futures::poll!(next.as_mut()).is_pending());
    }

    receiver.try_recv().unwrap();

    assert_eq!(queue.queued_bytes(), 0);
    assert!(
        queue
            .send_bounded(Outbound::new(0, kind::CONTROL_JSON, vec![2; 256]))
            .await
    );
    assert_eq!(receiver.try_recv().unwrap().payload, vec![2; 256]);
}

#[tokio::test]
async fn large_control_transfer_is_reassembled_without_exceeding_queue_budget() {
    let (queue, mut receiver) = SendQueue::new();

    let source: Vec<u8> = (0..10 * 1024 * 1024)
        .map(|index| (index % 251) as u8)
        .collect();

    let expected = source.clone();
    let sending = queue.clone();

    let task = tokio::spawn(async move {
        sending
            .send_chunked_control(Outbound::new(CONTROL_STREAM, kind::CONTROL_JSON, source))
            .await
    });

    let mut chunks = ControlChunks::default();

    let reconstructed = timeout(Duration::from_secs(5), async {
        loop {
            assert!(queue.queued_bytes() <= MAX_QUEUED / 2);

            if let Some(message) = receiver.try_recv() {
                assert_eq!(message.kind, kind::CONTROL_CHUNK);

                if let Some(bytes) = chunks.push(&message.payload).unwrap() {
                    break bytes;
                }
            } else {
                yield_now().await;
            }
        }
    })
    .await
    .unwrap();

    assert!(task.await.unwrap());
    assert_eq!(reconstructed, expected);
    assert_eq!(queue.queued_bytes(), 0);
}

fn control_chunk(id: u64, total: u64, offset: u64, bytes: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();

    payload.extend_from_slice(&id.to_be_bytes());
    payload.extend_from_slice(&total.to_be_bytes());
    payload.extend_from_slice(&offset.to_be_bytes());
    payload.extend_from_slice(bytes);

    payload
}

#[test]
fn control_chunks_reject_malformed_sizes_and_sequences() {
    let mut chunks = ControlChunks::default();

    assert!(chunks.push(&[]).is_err());
    assert!(
        chunks
            .push(&control_chunk(1, MAX_CONTROL_TRANSFER as u64 + 1, 0, b"a"))
            .is_err()
    );
    assert!(chunks.push(&control_chunk(1, 3, 1, b"a")).is_err());
    assert!(
        chunks
            .push(&control_chunk(1, 3, 0, b"a"))
            .unwrap()
            .is_none()
    );
    assert!(chunks.push(&control_chunk(1, 3, 0, b"a")).is_err());
    assert!(chunks.push(&control_chunk(1, 3, 2, b"b")).is_err());
    assert!(chunks.push(&control_chunk(2, 3, 1, b"b")).is_err());
    assert!(chunks.push(&control_chunk(1, 4, 1, b"b")).is_err());
    assert_eq!(
        chunks.push(&control_chunk(1, 3, 1, b"bc")).unwrap(),
        Some(b"abc".to_vec())
    );
}

#[test]
fn a_new_transfer_replaces_a_cancelled_partial_transfer() {
    let mut chunks = ControlChunks::default();

    assert!(
        chunks
            .push(&control_chunk(1, 100, 0, b"ab"))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        chunks.push(&control_chunk(2, 3, 0, b"new")).unwrap(),
        Some(b"new".to_vec())
    );
}

#[tokio::test]
async fn oversized_bounded_payload_is_rejected_before_queueing() {
    let (queue, _receiver) = SendQueue::new();

    assert!(
        !queue
            .send_bounded(Outbound::new(
                0,
                kind::CONTROL_JSON,
                vec![0; MAX_QUEUED / 2 + 1]
            ))
            .await
    );
    assert_eq!(queue.queued_bytes(), 0);
}

#[tokio::test]
async fn large_control_chunks_cross_the_encrypted_channel() {
    let (client_channel, host_channel) = channels();
    let (client_ws, host_ws) = sockets().await;
    let (client_out, client_out_rx) = SendQueue::new();
    let (_host_out, host_out_rx) = SendQueue::new();
    let (client_in, _client_in_rx) = mpsc::unbounded_channel();

    let (host_in, mut host_in_rx) = mpsc::unbounded_channel();

    let client = tokio::spawn(pump(
        client_ws,
        client_channel,
        client_out_rx,
        client_in,
        Arc::default(),
    ));

    let host = tokio::spawn(pump(
        host_ws,
        host_channel,
        host_out_rx,
        host_in,
        Arc::default(),
    ));

    let source = vec![b'x'; MAX_QUEUED + 1024];
    let expected_len = source.len();
    let sending = client_out.clone();

    let transfer = tokio::spawn(async move {
        sending
            .send_chunked_control(Outbound::new(CONTROL_STREAM, kind::CONTROL_JSON, source))
            .await
    });

    let received = timeout(Duration::from_secs(30), async {
        let mut chunks = ControlChunks::default();

        loop {
            let message = host_in_rx
                .recv()
                .await
                .expect("the encrypted channel stays open");

            assert_eq!(message.kind, kind::CONTROL_CHUNK);
            assert!(client_out.queued_bytes() <= MAX_QUEUED / 2);

            if let Some(bytes) = chunks.push(&message.payload).unwrap() {
                break bytes;
            }
        }
    })
    .await
    .unwrap();

    assert!(transfer.await.unwrap());
    assert_eq!(received.len(), expected_len);
    assert!(received.iter().all(|byte| *byte == b'x'));
    assert!(!client.is_finished());
    assert!(!host.is_finished());

    client.abort();
    host.abort();
}
