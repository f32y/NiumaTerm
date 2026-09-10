use serde_json::json;

use crate::codex::app_server::host::early::EarlyMessages;

#[test]
fn delayed_ownership_retains_every_thread_and_message_in_order() {
    let mut early = EarlyMessages::default();

    for thread in 0..80 {
        for id in 0..80 {
            early.hold(&thread.to_string(), json!({"id": id}));
        }
    }

    for thread in 0..80 {
        let messages = early.take(&thread.to_string());

        assert_eq!(messages.len(), 80);

        for (id, message) in messages.iter().enumerate() {
            assert_eq!(message["id"], id);
        }

        assert!(early.take(&thread.to_string()).is_empty());
    }

    assert!(early.threads.is_empty());
}

#[test]
fn large_payloads_survive_previous_per_thread_and_shared_byte_limits() {
    let mut early = EarlyMessages::default();

    for thread in 0..9 {
        early.hold(
            &thread.to_string(),
            json!({"text": "x".repeat(3 * 1024 * 1024)}),
        );
    }

    for thread in 0..9 {
        let messages = early.take(&thread.to_string());

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0]["text"].as_str().unwrap(),
            "x".repeat(3 * 1024 * 1024)
        );
    }

    assert!(early.threads.is_empty());
}

#[test]
fn explicit_thread_and_host_cleanup_release_retained_messages() {
    let mut early = EarlyMessages::default();

    early.hold("closed", json!({"id": 1}));
    early.hold("live", json!({"id": 2}));
    early.forget("closed");

    assert!(early.take("closed").is_empty());
    assert_eq!(early.take("live"), vec![json!({"id": 2})]);

    early.hold("shutdown", json!({"id": 3}));
    early.clear();

    assert!(early.threads.is_empty());
}
