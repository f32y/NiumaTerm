use futures::StreamExt as _;
use futures::executor::block_on;
use serde_json::json;

use crate::session::inbox::{MAX_BYTES, MAX_MESSAGES, channel};

#[test]
fn overflow_keeps_accepted_messages_then_ends_with_one_error() {
    let (sender, mut receiver) = channel();
    for index in 0..MAX_MESSAGES {
        sender.send(json!(index));
    }
    sender.send(json!("overflow"));
    sender.send(json!("late"));
    for index in 0..MAX_MESSAGES {
        let mut message = block_on(receiver.next()).unwrap().unwrap();
        assert_eq!(message.take(), json!(index));
    }
    assert!(block_on(receiver.next()).unwrap().is_err());
    assert!(block_on(receiver.next()).is_none());
}

#[test]
fn consumed_messages_release_budget_and_large_messages_fail_closed() {
    let (sender, mut receiver) = channel();
    for _ in 0..MAX_MESSAGES * 2 {
        sender.send(json!("text"));
        drop(block_on(receiver.next()).unwrap().unwrap());
    }
    sender.send(json!("x".repeat(MAX_BYTES)));
    assert!(block_on(receiver.next()).unwrap().is_err());
    assert!(block_on(receiver.next()).is_none());
}
