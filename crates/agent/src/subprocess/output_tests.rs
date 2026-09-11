use std::io::{BufReader, Cursor};

use serde_json::json;

use crate::subprocess::read_messages;

#[test]
fn large_history_reply_preserves_following_messages() {
    let history = "x".repeat(9 * 1024 * 1024);

    let input = format!(
        "{}\n{{\"next\":true}}\n",
        json!({"result":{"history":history}})
    );

    let mut reader = BufReader::new(Cursor::new(input));
    let mut messages = Vec::new();

    read_messages(&mut reader, "Test", |message| messages.push(message)).unwrap();

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["result"]["history"].as_str().unwrap(), history);
    assert_eq!(messages[1], json!({"next":true}));
}

#[test]
fn startup_notices_bom_and_blank_lines_preserve_protocol_objects() {
    let mut reader = Cursor::new(b"\xef\xbb\xbfStarting agent\r\n[WARN] startup notice\n\n{\"ready\":true}\r\n  \n{\"done\":true}");
    let mut messages = Vec::new();

    read_messages(&mut reader, "Test", |message| messages.push(message)).unwrap();

    assert_eq!(messages, [json!({"ready":true}), json!({"done":true})]);
}

#[test]
fn malformed_protocol_stops_before_later_messages_without_exposing_input() {
    let mut reader =
        Cursor::new(b"{\"ready\":true}\n{\"token\":\"private-value\",\n{\"late\":true}\n");

    let mut messages = Vec::new();
    let error = read_messages(&mut reader, "Test", |message| messages.push(message)).unwrap_err();

    assert_eq!(messages, [json!({"ready":true})]);
    assert!(error.contains("JSON is invalid"));
    assert!(!error.contains("private-value"));
    assert!(!error.contains("token"));
}

#[test]
fn malformed_startup_json_invalid_utf8_and_non_objects_fail() {
    for bytes in [
        b"{broken\n".as_slice(),
        b"[1,]\n",
        b"\xff\n",
        b"null\n",
        b"[]\n",
        b"42\n",
        b"\"text\"\n",
        b"{}\nlate notice\n",
    ] {
        assert!(read_messages(&mut Cursor::new(bytes), "Test", |_| {}).is_err());
    }
}

#[test]
fn long_startup_notices_preserve_the_first_protocol_message() {
    let input = format!(
        "{}{}\n{{\"ready\":true}}",
        "notice\n".repeat(32),
        "n".repeat(128 * 1024)
    );

    let mut messages = Vec::new();

    read_messages(&mut Cursor::new(input), "Test", |message| {
        messages.push(message)
    })
    .unwrap();

    assert_eq!(messages, [json!({"ready":true})]);
}
