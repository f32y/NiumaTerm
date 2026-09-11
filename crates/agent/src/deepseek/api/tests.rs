use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use tungstenite::accept_hdr;
use tungstenite::handshake::server::Request;

use crate::deepseek::api::{ApiClient, CallError};
use crate::deepseek::commands;

fn read_request(stream: &TcpStream) -> (String, String, Value) {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();

    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();

    reader.read_line(&mut line).unwrap();

    let mut headers = String::new();
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

        headers.push_str(&header.to_ascii_lowercase());
    }

    let mut body = vec![0; length];

    reader.read_exact(&mut body).unwrap();

    let body = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap()
    };

    (line, headers, body)
}

#[test]
#[expect(
    clippy::result_large_err,
    reason = "The handshake callback must return tungstenite's fixed response type."
)]
fn startup_token_authenticates_rpc_and_stream_without_corrupting_the_path() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut login, _) = listener.accept().unwrap();
        let (line, _, _) = read_request(&login);

        assert_eq!(line, "GET /?token=startup-secret HTTP/1.1\r\n");

        write!(
            login,
            "HTTP/1.1 303 See Other\r\nLocation: /\r\nSet-Cookie: dsh-auth=session-cookie; HttpOnly; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();

        let (mut rpc, _) = listener.accept().unwrap();
        let (line, headers, body) = read_request(&rpc);

        assert_eq!(line, "POST /api/session/create HTTP/1.1\r\n");
        assert!(headers.contains("cookie: dsh-auth=session-cookie\r\n"));
        assert_eq!(body["method"], "session/create");
        assert_eq!(
            body["payload"],
            json!({ "args": { "request": { "cwd": "project" } } })
        );

        let answer = r#"{"result":{"ok":true,"value":{"sessionId":"session-1"}}}"#;

        write!(
            rpc,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
            answer.len()
        )
        .unwrap();

        let (socket, _) = listener.accept().unwrap();

        let mut socket = accept_hdr(socket, |request: &Request, response| {
            assert_eq!(request.uri(), "/api/remote.mux");
            assert_eq!(request.headers()["cookie"], "dsh-auth=session-cookie");
            Ok(response)
        })
        .unwrap();

        let _ = socket.close(None);
    });

    let client = ApiClient::new(format!("http://{address}/?token=startup-secret")).unwrap();

    assert_eq!(
        client
            .request("session/create", json!({ "cwd": "project" }))
            .unwrap()["sessionId"],
        "session-1"
    );

    let _ = tungstenite::connect(client.stream_request().unwrap()).unwrap();

    server.join().unwrap();
}

#[test]
fn event_reply_keeps_the_generation_and_event_ids() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (line, _, body) = read_request(&stream);

        assert_eq!(line, "POST /api/$events/result HTTP/1.1\r\n");
        assert_eq!(
            body["payload"],
            json!({ "args": {
            "clientId": "generation-1", "eventId": "approval-1",
            "outcome": { "kind": "result", "value": "allowed-once" },
        } })
        );

        let answer = r#"{"result":{"ok":true}}"#;

        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
            answer.len()
        )
        .unwrap();
    });

    let client = ApiClient::new(format!("http://{address}")).unwrap();

    client
        .respond_event(
            "generation-1",
            "approval-1",
            json!({ "kind": "result", "value": "allowed-once" }),
        )
        .unwrap();
    server.join().unwrap();
}

fn command_server(replies: Vec<(Value, Value)>) -> (ApiClient, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        for (arguments, answer) in replies {
            let (mut stream, _) = listener.accept().unwrap();
            let (line, _, body) = read_request(&stream);
            assert_eq!(line, "POST /api/commands/execute HTTP/1.1\r\n");
            assert_eq!(body["method"], "commands/execute");
            assert_eq!(body["payload"]["args"], arguments);

            let answer = answer.to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                answer.len()
            )
            .unwrap();
        }
    });
    (ApiClient::new(format!("http://{address}")).unwrap(), server)
}

#[test]
fn commands_submit_an_empty_attachment_list_for_every_command_line() {
    let lines = [
        "/permission dangerously",
        "/permission",
        "/compact keep the design",
    ];
    let value = json!({"commandId": "command-1", "result": {"kind": "success"}});
    let (client, server) = command_server(
        lines
            .iter()
            .map(|line| {
                (
                    json!({"agentId": "session-1", "line": line, "submittedAttachments": []}),
                    json!({"result": {"ok": true, "value": value}}),
                )
            })
            .collect(),
    );
    for line in lines {
        assert_eq!(
            commands::execute(&client, "session-1", line).unwrap(),
            value
        );
    }
    server.join().unwrap();
}

const LEGACY_ARGUMENT_ERROR: &str = "typert gateway: commands/execute: args fields do not match the descriptor: missing \"images\"; unexpected \"submittedAttachments\"";

#[test]
fn commands_retry_the_older_attachment_name_after_argument_rejection() {
    let line = "/permission dangerously";
    let value = json!({"commandId": "command-1", "result": {"kind": "success"}});
    let (client, server) = command_server(vec![
        (
            json!({"agentId": "session-1", "line": line, "submittedAttachments": []}),
            json!({"result": {"ok": false, "error": {
                "code": "gateway/arguments-invalid", "message": LEGACY_ARGUMENT_ERROR,
            }}}),
        ),
        (
            json!({"agentId": "session-1", "line": line, "images": []}),
            json!({"result": {"ok": true, "value": value}}),
        ),
    ]);
    assert_eq!(
        commands::execute(&client, "session-1", line).unwrap(),
        value
    );
    server.join().unwrap();
}

#[test]
fn commands_return_unrelated_failures_without_retrying() {
    for (code, message) in [
        ("gateway/internal", LEGACY_ARGUMENT_ERROR),
        ("gateway/arguments-invalid", "another argument is invalid"),
        ("command/failed", "the command failed after starting"),
    ] {
        let (client, server) = command_server(vec![(
            json!({"agentId": "session-1", "line": "/permission dangerously", "submittedAttachments": []}),
            json!({"result": {"ok": false, "error": {"code": code, "message": message}}}),
        )]);
        assert_eq!(
            commands::execute(&client, "session-1", "/permission dangerously"),
            Err(CallError::Business {
                code: code.into(),
                message: message.into()
            })
        );
        server.join().unwrap();
    }
}
