use std::mem::size_of;

use serde_json::{Value, json};

use crate::subprocess::input::{
    InputClass, InputError, InputQueue, MAX_BYTES, MAX_MESSAGES, RESERVED_BYTES, RESERVED_MESSAGES,
};

#[test]
fn normal_bytes_cannot_consume_the_control_reserve() {
    let (queue, receiver) = InputQueue::new();
    let normal = Value::String("n".repeat(MAX_BYTES - RESERVED_BYTES - size_of::<Value>()));
    queue.submit(vec![normal], InputClass::Normal).unwrap();
    assert_eq!(
        queue.submit(vec![Value::Null], InputClass::Normal),
        Err(InputError::ByteLimit)
    );
    let control = Value::String("c".repeat(RESERVED_BYTES - size_of::<Value>()));
    queue.submit(vec![control], InputClass::Control).unwrap();
    assert_eq!(
        queue.submit(vec![Value::Null], InputClass::Control),
        Err(InputError::ByteLimit)
    );
    drop(receiver);
    assert_eq!(queue.budget.lock().bytes, 0);
}

#[test]
fn normal_saturation_leaves_control_slots_and_keeps_fifo_order() {
    let (queue, receiver) = InputQueue::new();
    let normal = MAX_MESSAGES - RESERVED_MESSAGES;
    for index in 0..normal {
        queue
            .submit(vec![json!(index)], InputClass::Normal)
            .unwrap();
    }
    let writing = receiver.recv().unwrap();
    assert_eq!(
        queue.submit(vec![json!("rejected")], InputClass::Normal),
        Err(InputError::MessageLimit)
    );
    for index in normal..MAX_MESSAGES {
        queue
            .submit(vec![json!(index)], InputClass::Control)
            .unwrap();
    }
    assert_eq!(
        queue.submit(vec![json!("rejected control")], InputClass::Control),
        Err(InputError::MessageLimit)
    );
    assert_eq!(writing.messages, [json!(0)]);
    drop(writing);
    for index in 1..MAX_MESSAGES {
        assert_eq!(receiver.recv().unwrap().messages, [json!(index)]);
    }
    queue
        .submit(vec![json!("retry")], InputClass::Normal)
        .unwrap();
}

#[test]
fn byte_saturation_is_recoverable_and_includes_the_active_write() {
    let (queue, receiver) = InputQueue::new();
    let payload = || Value::String("x".repeat(MAX_BYTES / 2));
    queue.submit(vec![payload()], InputClass::Normal).unwrap();
    let writing = receiver.recv().unwrap();
    assert_eq!(
        queue.submit(vec![payload()], InputClass::Normal),
        Err(InputError::ByteLimit)
    );
    queue
        .submit(vec![json!({"interrupt":true})], InputClass::Control)
        .unwrap();
    drop(writing);
    queue.submit(vec![payload()], InputClass::Normal).unwrap();
    assert_eq!(
        receiver.recv().unwrap().messages,
        [json!({"interrupt":true})]
    );
}

#[test]
fn oversized_payloads_and_failed_batches_leave_no_partial_input() {
    let (queue, receiver) = InputQueue::new();
    assert!(matches!(
        queue.submit(
            vec![Value::String("x".repeat(MAX_BYTES))],
            InputClass::Normal
        ),
        Err(InputError::TooLarge { .. })
    ));
    assert!(receiver.try_recv().is_err());
    for _ in 0..MAX_MESSAGES - RESERVED_MESSAGES - 1 {
        queue
            .submit(vec![json!("prior")], InputClass::Normal)
            .unwrap();
    }
    assert_eq!(
        queue.submit(vec![json!("settings"), json!("user")], InputClass::Normal),
        Err(InputError::MessageLimit)
    );
    queue
        .submit(vec![json!("retry")], InputClass::Normal)
        .unwrap();
    let accepted: Vec<_> = receiver
        .try_iter()
        .flat_map(|input| input.messages)
        .collect();
    assert!(!accepted.contains(&json!("settings")));
    assert_eq!(accepted.last(), Some(&json!("retry")));
}

#[test]
fn submission_moves_strings_and_releases_budget_on_disconnect() {
    let (queue, receiver) = InputQueue::new();
    let payload = "original allocation".to_string();
    let address = payload.as_ptr();
    queue
        .submit(vec![Value::String(payload)], InputClass::Normal)
        .unwrap();
    let input = receiver.recv().unwrap();
    assert_eq!(input.messages[0].as_str().unwrap().as_ptr(), address);
    drop(input);
    drop(receiver);
    assert_eq!(
        queue.submit(vec![json!("closed")], InputClass::Normal),
        Err(InputError::Closed)
    );
    assert_eq!(queue.budget.lock().bytes, 0);
    assert_eq!(queue.budget.lock().messages, 0);
}
