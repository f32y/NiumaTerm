use std::sync::Arc;
use std::time::Duration;

use nmt_remote_core::identity::DeviceKey;
use serde_json::Value;
use tokio::time::timeout;

use crate::connection::{AgentUpdate, RemoteHost, Retry};
use crate::store::PairedHost;

fn host_without_transport() -> Arc<RemoteHost> {
    let key = DeviceKey::generate().unwrap();
    let record = PairedHost::new(*key.public(), "Test host".into(), None);

    RemoteHost::unstarted(
        record,
        Arc::new(DeviceKey::generate().unwrap()),
        "test".into(),
        Retry::Forever,
        |_| {},
    )
}

#[test]
fn an_expired_handle_cannot_detach_the_reopened_paged_view() {
    let host = host_without_transport();
    let (previous, _) = host.agent_view_paged("session".into());

    let (current, mut events) = host.agent_view_paged("session".into());

    drop(previous);

    assert!(host.paged_agents.lock().contains_key("session"));
    assert!(
        host.agents.lock()["session"]
            .send(AgentUpdate::Error("current view".into()))
            .is_ok()
    );
    assert!(
        matches!(events.try_recv().unwrap(), AgentUpdate::Error(message) if message == "current view")
    );

    drop(current);

    assert!(!host.agents.lock().contains_key("session"));
    assert!(!host.paged_agents.lock().contains_key("session"));
}

#[tokio::test]
async fn a_replaced_handle_cannot_request_history_for_the_new_view() {
    let host = host_without_transport();
    let (previous, _) = host.agent_view_paged("session".into());
    let (_current, _) = host.agent_view_paged("session".into());

    let error = timeout(
        Duration::from_millis(100),
        previous.load_earlier(Value::Null),
    )
    .await
    .unwrap()
    .unwrap_err();

    assert!(error.to_string().contains("replaced"));
}

#[test]
fn replacing_a_paged_view_with_a_legacy_view_clears_the_paging_mode() {
    let host = host_without_transport();
    let (previous, _) = host.agent_view_paged("session".into());
    let (current, _) = host.agent_view("session".into());

    assert!(!host.paged_agents.lock().contains_key("session"));

    drop(previous);

    assert!(host.agents.lock().contains_key("session"));

    drop(current);

    assert!(!host.agents.lock().contains_key("session"));
}
