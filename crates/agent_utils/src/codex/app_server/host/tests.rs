use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, channel, sync_channel};

use serde_json::{Value, json};

use crate::LaunchConfig;
use crate::codex::ProviderConfig;
use crate::codex::app_server::host::{
    HOST_INIT_RPC_ID, HostBootstrap, HostKey, Router, initialize_request, redact,
};

fn router() -> Router {
    let (startup_tx, _startup_rx) = sync_channel(1);
    Router::new(startup_tx)
}

fn register(router: &Router) -> (u64, Receiver<Value>) {
    let (tx, rx) = channel();
    let owner = router.register(Arc::new(move |message| {
        let _ = tx.send(message);
    }));
    (owner, rx)
}

fn start_request(local_id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": local_id,
        "method": "thread/start",
        "params": {},
    })
}

fn start_response(global_id: u64, thread_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": global_id,
        "result": {"thread": {"id": thread_id}},
    })
}

#[test]
fn host_initialize_enables_experimental_api() {
    assert_eq!(
        initialize_request(),
        json!({
            "jsonrpc": "2.0",
            "id": HOST_INIT_RPC_ID,
            "method": "initialize",
            "params": {
                "clientInfo": {"name": "NiumaTerm", "version": "0.1.0"},
                "capabilities": {"experimentalApi": true},
            },
        })
    );
}

#[test]
fn responses_return_to_their_owner_with_local_ids() {
    let router = router();
    let (first, first_rx) = register(&router);
    let (second, second_rx) = register(&router);
    let mut first_request = start_request(2);
    let mut second_request = start_request(2);

    router
        .prepare_outgoing(first, &mut first_request)
        .expect("first request should route");
    router
        .prepare_outgoing(second, &mut second_request)
        .expect("second request should route");
    let first_global = first_request["id"].as_u64().expect("first global id");
    let second_global = second_request["id"].as_u64().expect("second global id");
    assert_ne!(first_global, second_global);

    router.handle_message(start_response(second_global, "thread-b"));
    router.handle_message(start_response(first_global, "thread-a"));

    assert_eq!(second_rx.recv().expect("second response")["id"], 2);
    assert_eq!(first_rx.recv().expect("first response")["id"], 2);
    assert!(first_rx.try_recv().is_err());
    assert!(second_rx.try_recv().is_err());
}

#[test]
fn server_requests_are_checked_against_thread_ownership() {
    let router = router();
    let (first, first_rx) = register(&router);
    let (second, second_rx) = register(&router);
    let mut first_request = start_request(2);
    let mut second_request = start_request(2);
    router.prepare_outgoing(first, &mut first_request).unwrap();
    router
        .prepare_outgoing(second, &mut second_request)
        .unwrap();
    router.handle_message(start_response(
        first_request["id"].as_u64().unwrap(),
        "thread-a",
    ));
    router.handle_message(start_response(
        second_request["id"].as_u64().unwrap(),
        "thread-b",
    ));
    let _ = first_rx.recv().unwrap();
    let _ = second_rx.recv().unwrap();

    router.handle_message(json!({
        "id": 900,
        "method": "item/commandExecution/requestApproval",
        "params": {"threadId": "thread-a", "turnId": "turn-a"},
    }));
    router.handle_message(json!({
        "id": 901,
        "method": "item/commandExecution/requestApproval",
        "params": {"threadId": "thread-b", "turnId": "turn-b"},
    }));
    assert_eq!(first_rx.recv().unwrap()["id"], 900);
    assert_eq!(second_rx.recv().unwrap()["id"], 901);

    let mut wrong_answer = json!({"id": 900, "result": {"decision": "decline"}});
    assert!(router.prepare_outgoing(second, &mut wrong_answer).is_err());
    router
        .prepare_outgoing(first, &mut wrong_answer)
        .expect("owner should answer its request");
}

#[test]
fn root_notifications_are_isolated_and_process_notifications_are_shared() {
    let router = router();
    let (first, first_rx) = register(&router);
    let (second, second_rx) = register(&router);
    let mut first_request = start_request(2);
    let mut second_request = start_request(2);
    router.prepare_outgoing(first, &mut first_request).unwrap();
    router
        .prepare_outgoing(second, &mut second_request)
        .unwrap();
    router.handle_message(start_response(
        first_request["id"].as_u64().unwrap(),
        "thread-a",
    ));
    router.handle_message(start_response(
        second_request["id"].as_u64().unwrap(),
        "thread-b",
    ));
    let _ = first_rx.recv().unwrap();
    let _ = second_rx.recv().unwrap();

    router.handle_message(json!({
        "method": "turn/started",
        "params": {"threadId": "thread-a", "turn": {"id": "turn-a"}},
    }));
    assert_eq!(first_rx.recv().unwrap()["params"]["threadId"], "thread-a");
    assert!(second_rx.try_recv().is_err());

    router.handle_message(json!({"method": "skills/changed", "params": {}}));
    assert_eq!(first_rx.recv().unwrap()["method"], "skills/changed");
    assert_eq!(second_rx.recv().unwrap()["method"], "skills/changed");

    router.handle_message(json!({
        "method": "turn/started",
        "params": {"threadId": "unowned", "turn": {"id": "late"}},
    }));
    assert!(first_rx.try_recv().is_err());
    assert!(second_rx.try_recv().is_err());
}

#[test]
fn auxiliary_title_thread_activity_never_reaches_the_primary_registration() {
    let router = router();
    let (primary, primary_rx) = register(&router);
    let (title_worker, title_rx) = register(&router);
    let mut primary_request = start_request(2);
    let mut title_request = start_request(1);
    router
        .prepare_outgoing(primary, &mut primary_request)
        .unwrap();
    router
        .prepare_outgoing(title_worker, &mut title_request)
        .unwrap();
    router.handle_message(start_response(
        primary_request["id"].as_u64().unwrap(),
        "thread-primary",
    ));
    router.handle_message(start_response(
        title_request["id"].as_u64().unwrap(),
        "thread-title",
    ));
    let _ = primary_rx.recv().unwrap();
    let _ = title_rx.recv().unwrap();

    router.handle_message(json!({
        "method": "item/completed",
        "params": {
            "threadId": "thread-title",
            "turnId": "turn-title",
            "item": {"type": "agentMessage", "text": "generated"},
        },
    }));

    assert_eq!(
        title_rx.recv().unwrap()["params"]["threadId"],
        "thread-title"
    );
    assert!(primary_rx.try_recv().is_err());
}

#[test]
fn early_descendant_activity_waits_for_a_proven_owner() {
    let router = router();
    let (owner, rx) = register(&router);
    let mut root_request = start_request(2);
    router.prepare_outgoing(owner, &mut root_request).unwrap();
    router.handle_message(start_response(
        root_request["id"].as_u64().unwrap(),
        "root-a",
    ));
    let _ = rx.recv().unwrap();

    router.handle_message(json!({
        "method": "item/started",
        "params": {"threadId": "child-a", "item": {"type": "agentMessage"}},
    }));
    assert!(rx.try_recv().is_err());

    router.claim_descendants(owner, ["child-a".to_string()]);
    assert_eq!(
        rx.recv().expect("held child activity")["params"]["threadId"],
        "child-a"
    );
}

#[test]
fn thread_started_inherits_the_known_parent_owner() {
    let router = router();
    let (first, first_rx) = register(&router);
    let (second, second_rx) = register(&router);
    let mut first_request = start_request(2);
    let mut second_request = start_request(2);
    router.prepare_outgoing(first, &mut first_request).unwrap();
    router
        .prepare_outgoing(second, &mut second_request)
        .unwrap();
    router.handle_message(start_response(
        first_request["id"].as_u64().unwrap(),
        "root-a",
    ));
    router.handle_message(start_response(
        second_request["id"].as_u64().unwrap(),
        "root-b",
    ));
    let _ = first_rx.recv().unwrap();
    let _ = second_rx.recv().unwrap();

    router.handle_message(json!({
        "method": "thread/started",
        "params": {"thread": {"id": "child-a", "parentThreadId": "root-a"}},
    }));
    assert_eq!(first_rx.recv().unwrap()["method"], "thread/started");
    assert!(second_rx.try_recv().is_err());

    router.handle_message(json!({
        "method": "item/started",
        "params": {"threadId": "child-a", "item": {"type": "agentMessage"}},
    }));
    assert_eq!(first_rx.recv().unwrap()["params"]["threadId"], "child-a");
    assert!(second_rx.try_recv().is_err());
}

#[test]
fn detached_sessions_do_not_receive_late_responses() {
    let router = router();
    let (owner, rx) = register(&router);
    let mut request = start_request(2);
    router.prepare_outgoing(owner, &mut request).unwrap();
    router.detach(owner);
    router.handle_message(start_response(request["id"].as_u64().unwrap(), "late"));
    assert!(rx.try_recv().is_err());
}

#[test]
fn a_root_conflict_keeps_the_requesting_sessions_previous_root() {
    let router = router();
    let (first, first_rx) = register(&router);
    let (second, second_rx) = register(&router);
    let mut first_request = start_request(2);
    let mut second_request = start_request(2);
    router.prepare_outgoing(first, &mut first_request).unwrap();
    router
        .prepare_outgoing(second, &mut second_request)
        .unwrap();
    router.handle_message(start_response(
        first_request["id"].as_u64().unwrap(),
        "root-a",
    ));
    router.handle_message(start_response(
        second_request["id"].as_u64().unwrap(),
        "root-b",
    ));
    let _ = first_rx.recv().unwrap();
    let _ = second_rx.recv().unwrap();

    let mut conflicting_resume = json!({
        "id": 9,
        "method": "thread/resume",
        "params": {"threadId": "root-a"},
    });
    router
        .prepare_outgoing(second, &mut conflicting_resume)
        .unwrap();
    router.handle_message(start_response(
        conflicting_resume["id"].as_u64().unwrap(),
        "root-a",
    ));
    assert!(
        second_rx.recv().unwrap()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("another Agent Tab")
    );

    router.handle_message(json!({
        "method": "turn/started",
        "params": {"threadId": "root-b", "turn": {"id": "turn-b"}},
    }));
    assert_eq!(second_rx.recv().unwrap()["params"]["threadId"], "root-b");
}

#[test]
fn an_early_closed_thread_is_not_retained_after_owner_discovery() {
    let router = router();
    let (owner, rx) = register(&router);
    let mut root_request = start_request(2);
    router.prepare_outgoing(owner, &mut root_request).unwrap();
    router.handle_message(start_response(
        root_request["id"].as_u64().unwrap(),
        "root-a",
    ));
    let _ = rx.recv().unwrap();

    router.handle_message(json!({
        "method": "thread/closed",
        "params": {"threadId": "child-a"},
    }));
    router.claim_descendants(owner, ["child-a".to_string()]);
    assert_eq!(rx.recv().unwrap()["method"], "thread/closed");

    router.handle_message(json!({
        "method": "turn/started",
        "params": {"threadId": "child-a", "turn": {"id": "late"}},
    }));
    assert!(rx.try_recv().is_err());
}

#[test]
fn unexpected_stdout_close_notifies_sessions_but_expected_shutdown_does_not() {
    let unexpected = router();
    let (_owner, unexpected_rx) = register(&unexpected);
    unexpected.handle_stdout_closed();
    assert_eq!(
        unexpected_rx.recv().expect("unexpected exit notification")["method"],
        "nmt/codexHostExited"
    );

    let expected = router();
    let (_owner, expected_rx) = register(&expected);
    expected.expected_shutdown.store(true, Ordering::Release);
    expected.handle_stdout_closed();
    assert!(expected_rx.try_recv().is_err());
}

fn custom_launch(
    executable: &str,
    provider_id: &str,
    credential_name: &str,
    credential: &str,
) -> LaunchConfig {
    LaunchConfig {
        executable: executable.to_string(),
        provider: Some(ProviderConfig {
            id: provider_id.to_string(),
            name: provider_id.to_string(),
            base_url: format!("https://{provider_id}.example/v1"),
            api_key_env: Some(credential_name.to_string()),
        }),
        env: vec![(credential_name.to_string(), credential.to_string())],
        ..LaunchConfig::default()
    }
}

#[test]
fn gateway_credentials_do_not_change_host_identity() {
    let first = custom_launch("codex", "provider-a", "NMT_CODEX_A", "secret-a");
    let second = custom_launch("codex", "provider-b", "NMT_CODEX_B", "secret-b");
    let bootstrap = HostBootstrap::from_launches(&first, &[first.clone(), second.clone()])
        .expect("compatible providers should merge");
    let credential_names = ["NMT_CODEX_A".to_string(), "NMT_CODEX_B".to_string()]
        .into_iter()
        .collect();

    assert!(bootstrap.key == HostKey::from_launch(&second, &credential_names));
    assert_eq!(bootstrap.credential_hashes.len(), 2);
}

#[test]
fn conflicting_credential_values_are_rejected() {
    let first = custom_launch("codex", "provider-a", "NMT_CODEX_SHARED", "secret-a");
    let second = custom_launch("codex", "provider-b", "NMT_CODEX_SHARED", "secret-b");
    let error = HostBootstrap::from_launches(&first, &[first.clone(), second])
        .err()
        .expect("credential collision should fail");

    assert!(error.contains("NMT_CODEX_SHARED"));
    assert!(!error.contains("secret-a"));
    assert!(!error.contains("secret-b"));
}

#[test]
fn conflicting_provider_definitions_are_rejected() {
    let first = custom_launch("codex", "provider-a", "NMT_CODEX_SHARED", "same-secret");
    let mut second = custom_launch("codex", "provider-b", "NMT_CODEX_SHARED", "same-secret");
    second.provider.as_mut().unwrap().base_url = "https://other.example/v1".into();
    let error = HostBootstrap::from_launches(&first, &[first.clone(), second])
        .err()
        .expect("provider collision should fail");

    assert!(error.contains("NMT_CODEX_SHARED"));
    assert!(!error.contains("same-secret"));
}

#[test]
fn credential_values_are_redacted_from_host_diagnostics() {
    let text = redact(
        "gateway rejected secret-value and shorter",
        &["secret-value".to_string(), "abc".to_string()],
    );

    assert_eq!(text, "gateway rejected <redacted> and shorter");
    assert_eq!(
        redact("bad key abc", &["abc".to_string()]),
        "bad key <redacted>"
    );
}
