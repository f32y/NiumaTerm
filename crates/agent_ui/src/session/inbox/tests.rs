use futures::StreamExt as _;
use futures::executor::block_on;
use nmt_agent::message_memory::OUTPUT_FAILURE_METHOD;
use serde_json::json;

use crate::session::inbox::channel;

#[test]
fn large_history_and_message_burst_preserve_later_events() {
    let (sender, mut receiver) = channel();
    sender.send(json!({"history":"x".repeat(33 * 1024 * 1024)}));

    for index in 0..2048 {
        sender.send(json!({"index":index}));
    }

    drop(sender);
    let mut history = block_on(receiver.next()).unwrap().unwrap();
    assert_eq!(
        history.take()["history"].as_str().unwrap().len(),
        33 * 1024 * 1024
    );

    for index in 0..2048 {
        let mut message = block_on(receiver.next()).unwrap().unwrap();
        assert_eq!(message.take(), json!({"index":index}));
    }

    assert!(block_on(receiver.next()).is_none());
}

#[test]
fn protocol_failure_keeps_accepted_messages_then_ends_with_one_error() {
    let (sender, mut receiver) = channel();

    for index in 0..4 {
        sender.send(json!(index));
    }

    sender.send(json!({"method":OUTPUT_FAILURE_METHOD,"params":{"message":"invalid JSON"}}));
    sender.send(json!("late"));

    for index in 0..4 {
        let mut message = block_on(receiver.next()).unwrap().unwrap();
        assert_eq!(message.take(), json!(index));
    }

    assert!(matches!(block_on(receiver.next()).unwrap(), Err(error) if error == "invalid JSON"));
    assert!(block_on(receiver.next()).is_none());
}
