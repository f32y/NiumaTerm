use std::cell::RefCell;
use std::io::{BufRead as _, BufReader, Read as _};
use std::net::TcpListener;
use std::sync::{Arc, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::time::timeout;
use tungstenite::{Message, accept};

use crate::dsh::api::ApiClient;
use crate::dsh::events::{Downlinks, PassedEvent, Streams};
use crate::dsh::mapping::{approval_request, question_request};

fn item(stream: &str, value: Value) -> Value {
    json!({ "type": "item", "streamId": stream, "value": value })
}

#[test]
fn closing_downlinks_interrupts_handshakes_and_idle_reads_and_joins_delivery() {
    for ready in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();

        let client = nmt_runtime::handle()
            .block_on(ApiClient::new(format!(
                "http://{}",
                listener.local_addr().unwrap()
            )))
            .unwrap();

        let (reading, entered) = mpsc::channel();

        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();

            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();

            if ready {
                let mut socket = accept(stream).unwrap();

                for _ in 0..3 {
                    socket.read().unwrap();
                }

                for frame in [
                    item(
                        "events",
                        json!({ "type": "ready", "clientId": "generation" }),
                    ),
                    item("control", json!({ "type": "baseline", "value": {} })),
                    item("follow", json!({ "type": "snapshot", "records": [] })),
                ] {
                    socket
                        .send(Message::Text(frame.to_string().into()))
                        .unwrap();
                }

                reading.send(()).unwrap();

                let _ = socket.read();
            } else {
                let mut reader = BufReader::new(stream);

                reader.read_line(&mut String::new()).unwrap();

                reading.send(()).unwrap();

                let _ = reader.read_to_end(&mut Vec::new());
            }
        });

        let retained = Arc::new(());
        let delivery = Arc::clone(&retained);

        let (downlinks, connected) = Downlinks::spawn(
            client,
            Weak::new(),
            "session-1".into(),
            Arc::new(move |_| {
                let _ = &delivery;
            }),
        );

        entered.recv_timeout(Duration::from_secs(3)).unwrap();

        if ready {
            nmt_runtime::handle()
                .block_on(async { timeout(Duration::from_secs(3), connected).await })
                .unwrap()
                .unwrap()
                .unwrap();
        }

        let started = Instant::now();

        drop(downlinks);

        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(Arc::strong_count(&retained), 1, "delivery must have exited");

        server.join().unwrap();
    }
}

#[test]
fn a_lost_stream_without_a_serving_host_reports_the_host_exit() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();

    let client = nmt_runtime::handle()
        .block_on(ApiClient::new(format!(
            "http://{}",
            listener.local_addr().unwrap()
        )))
        .unwrap();

    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();

        let mut socket = accept(stream).unwrap();

        for _ in 0..3 {
            socket.read().unwrap();
        }

        for frame in [
            item(
                "events",
                json!({ "type": "ready", "clientId": "generation" }),
            ),
            item("control", json!({ "type": "baseline", "value": {} })),
            item("follow", json!({ "type": "snapshot", "records": [] })),
        ] {
            socket
                .send(Message::Text(frame.to_string().into()))
                .unwrap();
        }

        // Dropping the listener with the socket leaves nothing to reconnect to,
        // which is what a host that exited looks like from the tab.
    });

    let (frames_tx, frames) = mpsc::channel();

    let (_downlinks, _) = nmt_runtime::handle()
        .block_on(Downlinks::open(
            client,
            Weak::new(),
            "session-1".into(),
            Arc::new(move |frame: Value| {
                let _ = frames_tx.send(frame);
            }),
        ))
        .unwrap();

    server.join().unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);

    let exited = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());

        match frames.recv_timeout(remaining) {
            Ok(frame) if frame["payload"]["type"] == "nmt/host-exited" => break Some(frame),
            Ok(_) => {}
            Err(_) => break None,
        }
    };

    assert_eq!(
        exited.expect("the tab must be told the host exited")["payload"]["sessionId"],
        "session-1"
    );
}

#[test]
fn readiness_waits_for_all_subscriptions_and_delivers_the_opening_history() {
    let frames = RefCell::new(Vec::new());
    let deliver = |frame| frames.borrow_mut().push(frame);

    let mut streams = Streams::new("session-1");

    streams
        .process(
            item(
                "events",
                json!({ "type": "ready", "clientId": "generation-1" }),
            ),
            &deliver,
        )
        .unwrap();

    streams
        .process(
            item(
                "control",
                json!({ "type": "baseline", "value": { "queues": {}, "projections": {} } }),
            ),
            &deliver,
        )
        .unwrap();

    assert!(streams.ready_snapshot().is_none());

    let snapshot = json!({ "type": "snapshot", "cursor": 4, "records": [], "projections": { "asOfSeq": 4, "values": {} } });

    streams
        .process(item("follow", snapshot.clone()), &deliver)
        .unwrap();

    assert_eq!(streams.ready_snapshot(), Some(&snapshot));
    assert_eq!(frames.borrow().last().unwrap()["payload"]["page"], snapshot);
}

#[test]
fn control_updates_do_not_cross_sessions_and_keep_their_cursor() {
    let frames = RefCell::new(Vec::new());
    let deliver = |frame| frames.borrow_mut().push(frame);

    let mut streams = Streams::new("session-1");

    streams
        .process(
            item(
                "control",
                json!({ "type": "queue", "sessionId": "session-2", "items": [] }),
            ),
            &deliver,
        )
        .unwrap();

    assert!(frames.borrow().is_empty());

    streams.process(item("control", json!({ "type": "projection", "sessionId": "session-1", "key": "title", "value": "New title", "seq": 9 })), &deliver).unwrap();

    assert_eq!(
        frames.borrow()[0]["payload"],
        json!({ "type": "session/projection", "sessionId": "session-1", "key": "title", "value": "New title", "seq": 9 })
    );
}

#[test]
fn job_lists_reach_only_their_own_conversation() {
    let frames = RefCell::new(Vec::new());
    let deliver = |frame| frames.borrow_mut().push(frame);

    let mut streams = Streams::new("session-1");

    let job = json!({ "id": "bash-1", "kind": "bash", "label": "sleep 60", "status": "running", "startedAt": 1 });

    streams
        .process(
            item(
                "control",
                json!({ "type": "baseline", "value": {
                    "queues": {}, "projections": {},
                    "jobs": { "session-1": [job.clone()], "session-2": [] },
                } }),
            ),
            &deliver,
        )
        .unwrap();

    streams
        .process(
            item(
                "control",
                json!({ "type": "jobs", "sessionId": "session-2", "jobs": [job.clone()] }),
            ),
            &deliver,
        )
        .unwrap();

    // Each later frame replaces the whole list, so an empty one is news too.
    streams
        .process(
            item(
                "control",
                json!({ "type": "jobs", "sessionId": "session-1", "jobs": [] }),
            ),
            &deliver,
        )
        .unwrap();

    let jobs: Vec<Value> = frames
        .borrow()
        .iter()
        .map(|frame| frame["payload"].clone())
        .filter(|payload| payload["type"] == "session/jobs")
        .collect();

    assert_eq!(
        jobs,
        vec![
            json!({ "type": "session/jobs", "sessionId": "session-1", "jobs": [job] }),
            json!({ "type": "session/jobs", "sessionId": "session-1", "jobs": [] }),
        ]
    );
}

#[test]
fn interactions_retain_the_generation_and_cancel_the_matching_card() {
    let frames = RefCell::new(Vec::new());
    let deliver = |frame| frames.borrow_mut().push(frame);

    let mut streams = Streams::new("session-1");

    streams
        .process(
            item(
                "events",
                json!({ "type": "ready", "clientId": "generation-1" }),
            ),
            &deliver,
        )
        .unwrap();

    streams.process(item("events", json!({
        "type": "waterfall", "event": "approval/request", "eventId": "approval-1", "agentId": "session-1",
        "request": { "toolName": "pwsh", "reason": "Write outside the workspace" },
    })), &deliver).unwrap();

    let approval = approval_request(&frames.borrow()[0], "session-1").unwrap();

    assert_eq!(approval.client_id, "generation-1");
    assert_eq!(approval.event_id, "approval-1");

    streams.process(item("events", json!({
        "type": "waterfall", "event": "user-questions/request", "eventId": "question-1", "agentId": "session-1",
        "request": { "questions": [{ "id": "q1", "question": "Continue?", "options": [{ "label": "Yes" }] }] },
    })), &deliver).unwrap();

    let (questions, _) = question_request(&frames.borrow()[1], "session-1").unwrap();

    assert_eq!(questions.event_id, "question-1");

    streams
        .process(
            item(
                "events",
                json!({ "type": "cancel", "eventId": "approval-1" }),
            ),
            &deliver,
        )
        .unwrap();

    assert_eq!(
        frames.borrow().last().unwrap()["payload"]["type"],
        "approval/resolved"
    );

    streams
        .process(
            item(
                "events",
                json!({ "type": "cancel", "eventId": "question-1" }),
            ),
            &deliver,
        )
        .unwrap();

    assert_eq!(
        frames.borrow().last().unwrap()["payload"]["type"],
        "question/resolved"
    );
}

#[test]
fn a_failed_subscription_is_a_startup_failure() {
    let error = Streams::new("session-1")
        .process(
            json!({
                "type": "error", "streamId": "follow", "error": { "message": "session not found" },
            }),
            &|_| {},
        )
        .unwrap_err();

    assert!(error.contains("session not found"));
}

#[test]
fn an_interaction_for_another_conversation_is_passed_without_being_shown() {
    let frames = RefCell::new(Vec::new());
    let deliver = |frame| frames.borrow_mut().push(frame);

    let mut streams = Streams::new("session-1");

    streams
        .process(
            item(
                "events",
                json!({ "type": "ready", "clientId": "generation-1" }),
            ),
            &deliver,
        )
        .unwrap();

    let passed = streams
        .process(
            item(
                "events",
                json!({
                    "type": "waterfall", "event": "approval/request", "eventId": "approval-9",
                    "agentId": "session-2", "request": { "toolName": "pwsh" },
                }),
            ),
            &deliver,
        )
        .unwrap();

    assert_eq!(
        passed,
        Some(PassedEvent {
            client_id: "generation-1".into(),
            event_id: "approval-9".into(),
        })
    );
    assert!(frames.borrow().is_empty());
}

/// Regression: the reply that passes an interaction on used to run on the
/// reader itself, so a host slow to answer it also stopped the heartbeat
/// replies and had the socket dropped as dead.
#[test]
fn a_stalled_pass_reply_does_not_stop_heartbeat_answers() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();

    let client = nmt_runtime::handle()
        .block_on(ApiClient::new(format!(
            "http://{}",
            listener.local_addr().unwrap()
        )))
        .unwrap();

    let (ponged_tx, ponged) = mpsc::channel();

    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();

        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();

        let mut socket = accept(stream).unwrap();

        for _ in 0..3 {
            socket.read().unwrap();
        }

        for frame in [
            item(
                "events",
                json!({ "type": "ready", "clientId": "generation" }),
            ),
            item("control", json!({ "type": "baseline", "value": {} })),
            item("follow", json!({ "type": "snapshot", "records": [] })),
            item(
                "events",
                json!({
                    "type": "waterfall", "event": "approval/request", "eventId": "approval-9",
                    "agentId": "session-2", "request": {},
                }),
            ),
        ] {
            socket
                .send(Message::Text(frame.to_string().into()))
                .unwrap();
        }

        // The pass reply arrives as a second connection and is never answered.
        let (stalled_reply, _) = listener.accept().unwrap();

        socket.send(Message::Ping(Vec::new().into())).unwrap();

        let ponged = loop {
            match socket.read() {
                Ok(Message::Pong(_)) => break true,
                Ok(_) => {}
                Err(_) => break false,
            }
        };

        ponged_tx.send(ponged).unwrap();

        drop(stalled_reply);
    });

    let (downlinks, _) = nmt_runtime::handle()
        .block_on(Downlinks::open(
            client,
            Weak::new(),
            "session-1".into(),
            Arc::new(|_: Value| {}),
        ))
        .unwrap();

    assert!(
        ponged.recv_timeout(Duration::from_secs(5)).unwrap(),
        "the reader stopped answering heartbeats while a reply was outstanding"
    );

    drop(downlinks);

    server.join().unwrap();
}
