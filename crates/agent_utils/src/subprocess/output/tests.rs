use std::io::{BufReader, Cursor};

use serde_json::json;

use crate::subprocess::output::{MAX_STARTUP_BYTES, MAX_STARTUP_LINES, read_messages, read_piece};

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
fn startup_allowance_is_bounded_by_lines_and_bytes() {
    let lines = "notice\n".repeat(MAX_STARTUP_LINES + 1);
    assert!(
        read_messages(&mut Cursor::new(lines), "Test", |_| {})
            .unwrap_err()
            .contains("allowance")
    );
    let bytes = "n".repeat(MAX_STARTUP_BYTES + 1);
    assert!(
        read_messages(&mut Cursor::new(bytes), "Test", |_| {})
            .unwrap_err()
            .contains("allowance")
    );
}

#[test]
fn long_unterminated_output_is_bounded_and_preserves_following_bytes() {
    let mut reader = BufReader::with_capacity(3, Cursor::new(b"123456789\nnext\nlast"));
    assert_eq!(read_piece(&mut reader, 5).unwrap(), b"12345");
    assert_eq!(read_piece(&mut reader, 5).unwrap(), b"6789\n");
    assert_eq!(read_piece(&mut reader, 5).unwrap(), b"next\n");
    assert_eq!(read_piece(&mut reader, 5).unwrap(), b"last");
    assert!(read_piece(&mut reader, 5).unwrap().is_empty());
}
