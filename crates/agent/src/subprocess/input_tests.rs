use std::sync::Barrier;
use std::thread;

use serde_json::{Value, json};

use crate::subprocess::input::{InputClosed, InputQueue};

#[test]
fn large_input_and_queued_burst_preserve_order_while_a_write_is_active() {
    let (queue, receiver) = InputQueue::new();

    queue.submit(vec![json!("active")]).unwrap();

    let writing = receiver.recv().unwrap();

    queue
        .submit(vec![json!("x".repeat(33 * 1024 * 1024))])
        .unwrap();

    for index in 0..2048 {
        queue.submit(vec![json!(index)]).unwrap();
    }

    assert_eq!(writing.messages, [json!("active")]);
    assert_eq!(
        receiver.recv().unwrap().messages[0].as_str().unwrap().len(),
        33 * 1024 * 1024
    );

    for index in 0..2048 {
        assert_eq!(receiver.recv().unwrap().messages, [json!(index)]);
    }
}

#[test]
fn cancelled_payloads_are_removed_while_a_write_is_active() {
    let (queue, receiver) = InputQueue::new();

    queue.submit(vec![json!("active")]).unwrap();

    let writing = receiver.recv().unwrap();

    for _ in 0..128 {
        let ticket = queue.submit_tracked(vec![json!("queued")]).unwrap();

        assert!(ticket.cancel());
        assert!(queue.queue.state.lock().pending.is_empty());
    }

    assert_eq!(writing.messages, [json!("active")]);
}

#[test]
fn cancellation_and_writer_start_have_exactly_one_winner() {
    for _ in 0..128 {
        let (queue, receiver) = InputQueue::new();
        let ticket = queue.submit_tracked(vec![json!("message")]).unwrap();
        let barrier = Barrier::new(2);

        thread::scope(|scope| {
            let cancel = scope.spawn(|| {
                barrier.wait();

                ticket.cancel()
            });

            barrier.wait();

            let writing = receiver.try_recv().ok();

            assert_ne!(cancel.join().unwrap(), writing.is_some());
        });

        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn sender_close_drains_accepted_input_then_wakes_the_receiver() {
    let (queue, receiver) = InputQueue::new();

    queue.submit(vec![json!(1)]).unwrap();

    drop(queue);

    assert_eq!(receiver.recv().unwrap().messages, [json!(1)]);
    assert!(receiver.recv().is_err());
}

#[test]
fn cancellation_wins_before_start_and_cannot_split_a_started_batch() {
    let (queue, receiver) = InputQueue::new();

    let ticket = queue
        .submit_tracked(vec![json!("settings"), json!({"type":"user"})])
        .unwrap();

    assert!(ticket.is_batch());
    assert!(ticket.cancel());
    assert!(ticket.cancel());
    assert!(receiver.try_recv().is_err());

    let ticket = queue
        .submit_tracked(vec![json!("settings"), json!({"type":"user"})])
        .unwrap();

    let writing = receiver.recv().unwrap();

    assert!(!ticket.cancel());
    assert_eq!(writing.messages.len(), 2);
}

#[test]
fn submission_moves_strings_and_disconnect_cancels_pending_input() {
    let (queue, receiver) = InputQueue::new();
    let payload = "original allocation".to_string();
    let address = payload.as_ptr();

    queue.submit(vec![Value::String(payload)]).unwrap();

    let input = receiver.recv().unwrap();

    assert_eq!(input.messages[0].as_str().unwrap().as_ptr(), address);

    let ticket = queue
        .submit_tracked(vec![json!("cancel on close")])
        .unwrap();

    drop(receiver);

    assert!(ticket.is_cancelled());
    assert_eq!(queue.submit(vec![json!("closed")]), Err(InputClosed));
}
