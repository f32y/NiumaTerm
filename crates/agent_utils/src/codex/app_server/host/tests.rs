use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, channel, sync_channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::LaunchConfig;
use crate::codex::ProviderConfig;
use crate::codex::app_server::host::{
    HOST_INIT_RPC_ID, HostBootstrap, HostKey, Router, initialize_request, redact,
};
use crate::request_policy::RequestClass;
use crate::subprocess::InputTicket;

#[test]
fn expired_queued_requests_report_not_sent_and_ignore_late_success() {
    let router = router();
    let (owner, rx) = register(&router);
    let mut request = json!({"id": 10, "method": "thread/start"});
    router.prepare_outgoing(owner, &mut request).unwrap();
    let global_id = request["id"].as_u64().unwrap();
    let ticket = InputTicket::queued_for_test(false);
    router.attach_input(global_id, ticket.clone());
    router.expire_requests(Instant::now() + Duration::from_secs(301));
    let response = rx.try_recv().unwrap();
    assert_eq!(response["id"], 10);
    assert_eq!(response["error"]["data"]["notSent"], true);
    assert!(ticket.cancel());
    router.handle_message(json!({"id":global_id,"result":{"thread":{"id":"late"}}}));
    assert!(rx.try_recv().is_err());
}

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

#[test]
fn unanswered_requests_reserve_controls_and_expire_by_class() {
    let router = router();
    let (owner, rx) = register(&router);
    let (other, other_rx) = register(&router);
    for id in 0..120 {
        router
            .prepare_outgoing(owner, &mut json!({"id": id, "method": "thread/list"}))
            .unwrap();
    }
    assert!(
        router
            .prepare_outgoing(owner, &mut json!({"id": 120, "method": "thread/read"}))
            .is_err()
    );
    for id in 120..128 {
        router
            .prepare_outgoing(owner, &mut json!({"id": id, "method": "turn/interrupt"}))
            .unwrap();
    }
    assert!(
        router
            .prepare_outgoing(owner, &mut json!({"id": 128, "method": "turn/interrupt"}))
            .is_err()
    );
    let mut mutation = json!({"id": 1, "method": "thread/fork"});
    router.prepare_outgoing(other, &mut mutation).unwrap();
    let now = Instant::now();
    router.expire_requests(now + Duration::from_secs(16));
    let controls: Vec<_> = rx.try_iter().collect();
    assert_eq!(controls.len(), 8);
    assert!(controls.iter().all(|response| {
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("result is unknown")
    }));
    assert!(other_rx.try_recv().is_err());
    router.expire_requests(now + Duration::from_secs(31));
    let queries: Vec<_> = rx.try_iter().collect();
    assert_eq!(queries.len(), 120);
    assert!(queries.iter().all(|response| {
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("retry the query")
    }));
    router
        .prepare_outgoing(owner, &mut json!({"id": 129, "method": "thread/read"}))
        .unwrap();
    router.expire_requests(now + RequestClass::Mutation.timeout() + Duration::from_secs(1));
    assert!(
        other_rx.try_recv().unwrap()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("result is unknown")
    );
    router.handle_message(json!({"id": mutation["id"], "result": {"thread": {"id": "late"}}}));
    assert!(other_rx.try_recv().is_err());
    router.expire_requests(now + Duration::from_secs(600));
    assert!(other_rx.try_recv().is_err());
}

#[test]
fn retired_requests_cannot_reassign_roots_or_affect_other_owners() {
    let router = router();
    let (owner, rx) = register(&router);
    let (other, other_rx) = register(&router);
    let mut old = start_request(10);
    let mut current = start_request(11);
    let mut independent = start_request(10);
    router.prepare_outgoing(owner, &mut old).unwrap();
    router.prepare_outgoing(owner, &mut current).unwrap();
    router.prepare_outgoing(other, &mut independent).unwrap();
    router.retain_requests(owner, &[11]);
    router.handle_message(json!({"id": old["id"], "result": {"thread": {"id": "stale"}}}));
    assert!(rx.try_recv().is_err());
    router.handle_message(json!({"id": current["id"], "result": {"thread": {"id": "chosen"}}}));
    assert_eq!(rx.try_recv().unwrap()["id"], 11);
    router.handle_message(
        json!({"id": independent["id"], "result": {"thread": {"id": "independent"}}}),
    );
    assert_eq!(other_rx.try_recv().unwrap()["id"], 10);
}

#[test]
fn shared_host_bounds_pending_requests_across_many_owners() {
    let router = router();
    let mut owners = Vec::new();
    for _ in 0..8 {
        let (owner, _) = register(&router);
        owners.push(owner);
        for id in 0..120 {
            router
                .prepare_outgoing(owner, &mut json!({"id": id, "method": "thread/list"}))
                .unwrap();
        }
    }
    let (ninth, _) = register(&router);
    assert!(
        router
            .prepare_outgoing(ninth, &mut json!({"id": 1, "method": "thread/list"}))
            .is_err()
    );
    for owner in owners {
        for id in 120..128 {
            router
                .prepare_outgoing(owner, &mut json!({"id": id, "method": "turn/interrupt"}))
                .unwrap();
        }
    }
    assert!(
        router
            .prepare_outgoing(ninth, &mut json!({"id": 2, "method": "turn/interrupt"}))
            .is_err()
    );
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
fn rejected_requests_release_routes_without_affecting_other_sessions() {
    let router = router();
    let (first, first_rx) = register(&router);
    let (second, second_rx) = register(&router);
    let mut rejected = start_request(2);
    let mut accepted = start_request(2);
    router.prepare_outgoing(first, &mut rejected).unwrap();
    router.prepare_outgoing(second, &mut accepted).unwrap();
    let id = rejected["id"].as_u64().unwrap();
    router.reject_outgoing(id);
    router.handle_message(start_response(id, "rejected"));
    assert!(first_rx.try_recv().is_err());
    router.handle_message(start_response(accepted["id"].as_u64().unwrap(), "accepted"));
    assert_eq!(
        second_rx.try_recv().unwrap()["result"]["thread"]["id"],
        "accepted"
    );
    assert!(router.alive.load(Ordering::Acquire));
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
