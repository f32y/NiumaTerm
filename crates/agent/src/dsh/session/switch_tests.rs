use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Weak, mpsc};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::{Message, accept};

use crate::dsh::api::ApiClient;
use crate::dsh::session::switch::{SwitchSlot, Switching, Target, switch_conversation};

/// Answer one Remote call on `stream` and report the method it named.
fn answer_call(mut stream: TcpStream, answer: &str) -> String {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut content_length = 0;

    loop {
        let mut line = String::new();

        reader.read_line(&mut line).unwrap();

        if line == "\r\n" {
            break;
        }

        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap();
        }
    }

    let mut body = vec![0; content_length];

    reader.read_exact(&mut body).unwrap();

    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
        answer.len()
    )
    .unwrap();

    serde_json::from_slice::<Value>(&body).unwrap()["method"]
        .as_str()
        .unwrap()
        .to_string()
}

fn item(stream: &str, value: Value) -> Value {
    json!({ "type": "item", "streamId": stream, "value": value })
}

fn switching(client: ApiClient, slot: &SwitchSlot) -> (Switching, mpsc::Receiver<Value>) {
    let (frames_tx, frames) = mpsc::channel();

    let switching = Switching {
        client,
        host: Weak::new(),
        cwd: None,
        current: "session-1".into(),
        deliver: Arc::new(move |frame| {
            let _ = frames_tx.send(frame);
        }),
        slot: Arc::clone(slot),
    };

    (switching, frames)
}

/// The opened streams report before the session has been told to accept their
/// conversation, and it drops frames for a conversation it is not on. What
/// they said has to reach it behind the announcement, in the order it was said.
#[test]
fn the_announcement_precedes_everything_the_new_streams_reported() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();

    let client = nmt_platform::runtime()
        .block_on(ApiClient::new(format!(
            "http://{}",
            listener.local_addr().unwrap()
        )))
        .unwrap();

    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();

        let method = answer_call(
            stream,
            r#"{"result":{"ok":true,"value":{"sessionId":"session-2"}}}"#,
        );

        assert_eq!(method, "session/create");

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
        ] {
            socket
                .send(Message::Text(frame.to_string().into()))
                .unwrap();
        }

        // Held open until the client lets go, so the reader stays connected
        // for as long as the test looks at what it delivered.
        let _ = socket.read();
    });

    let slot = SwitchSlot::default();
    let (switching, frames) = switching(client, &slot);

    nmt_platform::runtime().block_on(switch_conversation(
        switching,
        Target::Existing("session-2".into()),
    ));

    let delivered: Vec<Value> = frames.try_iter().collect();

    let kinds: Vec<&str> = delivered
        .iter()
        .map(|frame| frame["payload"]["type"].as_str().unwrap())
        .collect();

    assert_eq!(
        kinds,
        ["nmt/command-settled", "session/queue", "nmt/replay"]
    );
    assert_eq!(delivered[0]["payload"]["sessionId"], "session-2");
    assert_eq!(delivered[0]["payload"]["command"]["kind"], "switched");

    let switch = slot
        .lock()
        .take()
        .expect("the opened streams were handed over");

    assert_eq!(switch.opened.session_id, "session-2");

    drop(switch);

    server.join().unwrap();
}

#[test]
fn a_refused_change_is_reported_against_the_conversation_the_tab_is_on() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();

    let client = nmt_platform::runtime()
        .block_on(ApiClient::new(format!(
            "http://{}",
            listener.local_addr().unwrap()
        )))
        .unwrap();

    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();

        answer_call(
            stream,
            r#"{"result":{"ok":false,"error":{"code":"session/cwd-mismatch","message":"rooted elsewhere"}}}"#,
        );
    });

    let slot = SwitchSlot::default();
    let (switching, frames) = switching(client, &slot);

    nmt_platform::runtime().block_on(switch_conversation(
        switching,
        Target::Existing("session-2".into()),
    ));

    server.join().unwrap();

    let delivered: Vec<Value> = frames.try_iter().collect();

    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0]["payload"]["sessionId"], "session-1");
    assert_eq!(
        delivered[0]["payload"]["command"],
        json!({ "kind": "switchFailed", "error": "rooted elsewhere" })
    );
    assert!(slot.lock().is_none());
}
