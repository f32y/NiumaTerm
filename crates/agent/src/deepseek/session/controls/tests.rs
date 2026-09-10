use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::deepseek::api::ApiClient;
use crate::deepseek::mapping::ApprovalRequest;
use crate::deepseek::session::controls::{Controls, Operation};

fn read_request(stream: &TcpStream) -> Value {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();

    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut length = 0;

    loop {
        let mut header = String::new();

        reader.read_line(&mut header).unwrap();

        if header == "\r\n" {
            break;
        }

        if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
    }

    let mut body = vec![0; length];

    reader.read_exact(&mut body).unwrap();

    serde_json::from_slice(&body).unwrap()
}

fn reply(stream: &mut TcpStream, success: bool) {
    let answer = json!({"result": {"ok": success, "value": null,
        "error": {"code": "busy", "message": "Please retry"}}})
    .to_string();

    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
        answer.len()
    )
    .unwrap();
}

#[test]
fn stalled_http_accepts_large_payloads_and_a_burst_of_distinct_controls() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = ApiClient::new(format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();

        assert_eq!(read_request(&stream)["method"], "session/cancel");

        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        reply(&mut stream, true);
    });

    let (done_tx, done_rx) = mpsc::channel();

    let mut controls = Controls::new(
        client,
        Arc::new(move |value| {
            let _ = done_tx.send(value);
        }),
    )
    .unwrap();

    assert!(controls.submit(Operation::Interrupt, "session/cancel", json!({}), None));

    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();

    assert!(done_rx.try_recv().is_err());
    assert!(!controls.submit(Operation::Interrupt, "session/cancel", json!({}), None));
    assert!(controls.submit(
        Operation::InterruptChild("large".into()),
        "child",
        json!("x".repeat(64 * 1024)),
        None
    ));

    for id in 1..256 {
        assert!(controls.submit(
            Operation::InterruptChild(id.to_string()),
            "child",
            json!({}),
            None
        ));
    }

    assert!(controls.submit(
        Operation::InterruptChild("full".into()),
        "child",
        json!({}),
        None
    ));

    // Clearing while one HTTP request is blocked cancels every unstarted call.
    controls.clear();
    release_tx.send(()).unwrap();
    drop(controls);
    server.join().unwrap();

    assert!(done_rx.recv_timeout(Duration::from_secs(3)).is_err());
}

#[test]
fn failure_releases_admission_for_retry_and_old_completion_is_ignored() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = ApiClient::new(format!("http://{}", listener.local_addr().unwrap())).unwrap();

    let server = thread::spawn(move || {
        for success in [false, true] {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&stream);
            reply(&mut stream, success);
        }
    });

    let (tx, rx) = mpsc::channel();

    let mut controls = Controls::new(
        client,
        Arc::new(move |value| {
            tx.send(value).unwrap();
        }),
    )
    .unwrap();

    assert!(controls.submit(Operation::Interrupt, "session/cancel", json!({}), None));

    let failed = rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let first = failed["payload"]["id"].as_u64().unwrap();

    assert!(
        failed["payload"]["error"]
            .as_str()
            .unwrap()
            .contains("Please retry")
    );
    assert_eq!(controls.complete(first), Some(Operation::Interrupt));
    assert!(controls.submit(Operation::Interrupt, "session/cancel", json!({}), None));
    assert_eq!(controls.complete(first), None);

    let succeeded = rx.recv_timeout(Duration::from_secs(3)).unwrap();

    assert!(succeeded["payload"]["error"].is_null());

    controls.clear();

    assert_eq!(
        controls.complete(succeeded["payload"]["id"].as_u64().unwrap()),
        None
    );

    server.join().unwrap();
}

#[test]
fn cancelled_approval_reports_stop_failure_without_rejecting_the_answer() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = ApiClient::new(format!("http://{}", listener.local_addr().unwrap())).unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&stream);

        assert_eq!(request["method"], "$events/result");
        assert_eq!(request["payload"]["args"]["clientId"], "generation");
        assert_eq!(request["payload"]["args"]["eventId"], "event");

        reply(&mut stream, true);

        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&stream);

        assert_eq!(request["method"], "session/cancel");
        assert_eq!(
            request["payload"]["args"]["request"]["sessionId"],
            "session"
        );

        reply(&mut stream, false);
    });

    let (tx, rx) = mpsc::channel();

    let mut controls = Controls::new(
        client,
        Arc::new(move |value| {
            tx.send(value).unwrap();
        }),
    )
    .unwrap();

    assert!(controls.submit(
        Operation::Approval(ApprovalRequest { client_id: "generation".into(), event_id: "event".into(), description: "Run".into() }),
        "$events/result", json!({"clientId": "generation", "eventId": "event", "outcome": {"kind": "result", "value": "rejected"}}),
        Some("session".into()),
    ));

    let result = rx.recv_timeout(Duration::from_secs(3)).unwrap();

    assert!(result["payload"]["error"].is_null());
    assert_eq!(result["payload"]["stopError"], "Please retry");

    server.join().unwrap();
}
