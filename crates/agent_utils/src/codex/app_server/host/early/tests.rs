use serde_json::json;

use crate::codex::app_server::host::early::{
    EarlyMessages, MAX_BYTES, MAX_MESSAGES, MAX_THREAD_BYTES, MAX_THREADS,
};

#[test]
fn complete_replay_preserves_order_and_releases_all_bytes() {
    let mut early = EarlyMessages::default();
    for id in 0..MAX_MESSAGES {
        early.hold("thread", json!({"id": id}));
    }
    assert!(early.bytes > 0);
    let replay = early.take("thread");
    assert!(!replay.incomplete);
    assert_eq!(replay.messages.len(), MAX_MESSAGES);
    for (id, message) in replay.messages.iter().enumerate() {
        assert_eq!(message["id"], id);
    }
    assert_eq!(early.bytes, 0);
    assert!(early.order.is_empty());
}

#[test]
fn message_count_and_thread_bytes_each_mark_truncated_replay() {
    let mut early = EarlyMessages::default();
    for id in 0..=MAX_MESSAGES {
        early.hold("count", json!({"id": id}));
    }
    let replay = early.take("count");
    assert!(replay.incomplete);
    assert_eq!(replay.messages.len(), MAX_MESSAGES);
    assert_eq!(replay.messages[0]["id"], 1);
    for id in 0..4 {
        early.hold(
            "bytes",
            json!({"id": id, "text": "x".repeat(MAX_THREAD_BYTES / 3)}),
        );
        assert!(early.bytes <= MAX_THREAD_BYTES);
    }
    let replay = early.take("bytes");
    assert!(replay.incomplete);
    assert_eq!(replay.messages.len(), 2);
    assert_eq!(replay.messages[0]["id"], 2);
    assert_eq!(early.bytes, 0);
}

#[test]
fn shared_byte_limit_evicts_threads_but_keeps_loss_evidence() {
    let mut early = EarlyMessages::default();
    for id in 0..MAX_THREADS {
        early.hold(&id.to_string(), json!("x".repeat(MAX_THREAD_BYTES / 2)));
        assert!(early.bytes <= MAX_BYTES);
    }
    assert!(early.threads.len() < MAX_THREADS);
    let lost = early.take("0");
    assert!(lost.messages.is_empty());
    assert!(lost.incomplete);
    assert!(
        !early
            .take(&(MAX_THREADS - 1).to_string())
            .messages
            .is_empty()
    );
    early.clear();
    assert_eq!(early.bytes, 0);
    assert!(early.threads.is_empty());
    assert!(early.take("0").incomplete);
}

#[test]
fn thread_eviction_oversize_and_repeated_claims_cannot_hide_loss() {
    let mut early = EarlyMessages::default();
    for id in 0..=MAX_THREADS {
        early.hold(&id.to_string(), json!({"id": id}));
    }
    assert_eq!(early.threads.len(), MAX_THREADS);
    assert!(early.take("0").incomplete);
    early.hold("large", json!("x".repeat(MAX_THREAD_BYTES)));
    assert!(!early.threads.contains_key("large"));
    assert!(early.take("large").incomplete);
    early.hold("large", json!({"id": 42}));
    let replay = early.take("large");
    assert!(replay.incomplete);
    assert_eq!(replay.messages.len(), 1);
    early.forget("1");
    let sum: usize = early
        .threads
        .values()
        .map(|thread| thread.bytes + thread.overhead)
        .sum();
    assert_eq!(early.bytes, sum);
}
