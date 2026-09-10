use futures::StreamExt as _;
use futures::channel::mpsc;
use futures::executor::block_on;
use nmt_agent::chat::Event;

use crate::session::output::{EventBatch, MAX_MESSAGES_PER_BATCH};

fn text(item_id: &str, delta: &str) -> Event {
    Event::AgentMessageDelta {
        item_id: item_id.into(),
        delta: delta.into(),
    }
}

#[test]
fn a_large_ready_stream_preserves_text_with_fewer_updates() {
    let (sender, receiver) = mpsc::unbounded();

    for _ in 0..10_000 {
        sender.unbounded_send(text("answer", "piece ")).unwrap();
    }

    sender
        .unbounded_send(Event::TurnCompleted { error: None })
        .unwrap();
    drop(sender);

    let mut applied = Vec::new();

    block_on(async {
        let mut batches = receiver.ready_chunks(MAX_MESSAGES_PER_BATCH);

        while let Some(messages) = batches.next().await {
            assert!(messages.len() <= MAX_MESSAGES_PER_BATCH);

            let mut batch = EventBatch::default();

            for event in messages {
                batch.push(event, |event| applied.push(event));
            }

            batch.flush(|event| applied.push(event));
        }
    });

    assert_eq!(
        applied.len(),
        10_000_usize.div_ceil(MAX_MESSAGES_PER_BATCH) + 1
    );
    assert_eq!(applied.last(), Some(&Event::TurnCompleted { error: None }));

    let joined: String = applied
        .into_iter()
        .filter_map(|event| match event {
            Event::AgentMessageDelta { delta, .. } => Some(delta),
            _ => None,
        })
        .collect();

    assert_eq!(joined, "piece ".repeat(10_000));
}

#[test]
fn approvals_completion_and_errors_flush_prior_text_immediately() {
    let mut batch = EventBatch::default();
    let mut applied = Vec::new();

    batch.push(text("answer", "before "), |event| applied.push(event));
    batch.push(text("answer", "approval"), |event| applied.push(event));

    let approval = Event::ApprovalRequested {
        description: "Run the tool?".into(),
    };

    batch.push(approval.clone(), |event| applied.push(event));

    assert_eq!(applied, [text("answer", "before approval"), approval]);

    batch.push(text("answer", "after approval"), |event| {
        applied.push(event)
    });
    batch.push(Event::TurnCompleted { error: None }, |event| {
        applied.push(event)
    });

    let error = Event::Error {
        message: "connection closed".into(),
        fatal: true,
    };

    batch.push(error.clone(), |event| applied.push(event));

    assert_eq!(
        &applied[2..],
        [
            text("answer", "after approval"),
            Event::TurnCompleted { error: None },
            error
        ]
    );

    batch.flush(|event| applied.push(event));

    assert_eq!(applied.len(), 5);
}

#[test]
fn delta_types_and_item_ids_never_merge_across_boundaries() {
    let reasoning = |id: &str, delta: &str| Event::ReasoningSummaryDelta {
        item_id: id.into(),
        delta: delta.into(),
    };

    let command = |id: &str, delta: &str| Event::CommandOutputDelta {
        item_id: id.into(),
        delta: delta.into(),
    };

    let source = [
        text("a", "one"),
        text("b", "two"),
        reasoning("b", "think "),
        reasoning("b", "more"),
        command("b", "out "),
        command("b", "put"),
        text("a", "three"),
    ];

    let mut batch = EventBatch::default();
    let mut applied = Vec::new();

    for event in source {
        batch.push(event, |event| applied.push(event));
    }

    batch.flush(|event| applied.push(event));

    assert_eq!(
        applied,
        [
            text("a", "one"),
            text("b", "two"),
            reasoning("b", "think more"),
            command("b", "out put"),
            text("a", "three")
        ]
    );
}

#[test]
fn a_partial_batch_delivers_its_last_delta_before_eof() {
    let (sender, receiver) = mpsc::unbounded();

    sender
        .unbounded_send(text("answer", "last fragment"))
        .unwrap();
    drop(sender);

    let mut batches = receiver.ready_chunks(MAX_MESSAGES_PER_BATCH);
    let messages = block_on(batches.next()).unwrap();
    let mut batch = EventBatch::default();
    let mut applied = Vec::new();

    for event in messages {
        batch.push(event, |event| applied.push(event));
    }

    batch.flush(|event| applied.push(event));

    assert_eq!(applied, [text("answer", "last fragment")]);
    assert!(block_on(batches.next()).is_none());
}
