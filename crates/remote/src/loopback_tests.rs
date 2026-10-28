use std::collections::HashMap;
use std::env;
use std::future::poll_fn;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::task::Poll;
use std::time::Duration;

use nmt_config::CursorShape;
use nmt_config::colors::Colors;
use nmt_platform::{AsyncPty, PtyOptions, create_pty_with_env, runtime};
use nmt_remote_core::identity::DeviceKey;
use nmt_remote_core::messages::{DeviceInfo, DeviceKind, RelayAccess};
use nmt_remote_core::pairing::PairingCode;
use nmt_remote_core::push::{PushEnvironment, PushKind, PushRegistration};
use nmt_remote_core::rpc::{self, EndReason, Origin, SessionInfo, SessionKind};
use nmt_terminal::event::VoidListener;
use nmt_terminal::termio::{SessionHandles, SessionOptions, start_session};
use serde_json::{Value, json};
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::time::{Instant, sleep, timeout};

use crate::client::{LinkPath, PathPolicy, pair};
use crate::connection::{AgentUpdate, RemoteHost, Retry, Status};
use crate::host::{HostConfig, HostService};
use crate::lan::lan_addresses;
use crate::link::MAX_CONTROL_TRANSFER;
use crate::local_view;
use crate::netwatch::notify_changed as notify_network_changed;
use crate::network_pty::NetworkPty;
use crate::presence::Presence;
use crate::sessions::{AgentControl, AgentRequest, HostRequest, SessionRegistry, TerminalControl};
use crate::store::PairedHost;

const WAIT: Duration = Duration::from_secs(20);

fn info(name: &str) -> DeviceInfo {
    DeviceInfo {
        name: name.into(),
        kind: DeviceKind::Desktop,
        platform: "test".into(),
        app_version: "0.0.0".into(),
    }
}

/// A shell without prompt integration, so output is plain and fast to start.
fn test_shell() -> (Option<String>, Vec<String>) {
    if cfg!(windows) {
        (Some("cmd.exe".into()), vec!["/Q".into()])
    } else {
        (Some("/bin/sh".into()), vec!["-i".into()])
    }
}

/// A command whose output differs from the echo of its typed line.
fn marker_command(tag: &str) -> (Vec<u8>, String) {
    if cfg!(windows) {
        (
            format!("echo {tag}_%OS%_DONE\r").into_bytes(),
            format!("{tag}_Windows_NT_DONE"),
        )
    } else {
        (
            format!("echo {tag}_$((6*7))_DONE\r").into_bytes(),
            format!("{tag}_42_DONE"),
        )
    }
}

fn start_host(dir: &tempfile::TempDir, registry: Arc<SessionRegistry>) -> HostService {
    start_host_with_relay(dir, registry, None)
}

fn start_host_with_relay(
    dir: &tempfile::TempDir,
    registry: Arc<SessionRegistry>,
    relay: Option<RelayAccess>,
) -> HostService {
    let (shell, args) = test_shell();

    HostService::start(
        dir.path().to_path_buf(),
        DeviceKey::generate().unwrap(),
        HostConfig {
            port: 0,
            device: info("Host"),
            shell,
            args,
            registry,
            relay,
            announce: true,
            on_change: Arc::new(|| {}),
            stun_servers: Vec::new(),
        },
    )
    .unwrap()
}

async fn paired_client(host: &HostService) -> (PairedHost, Arc<DeviceKey>) {
    let address = format!("127.0.0.1:{}", host.local_addr().port());
    let key = DeviceKey::generate().unwrap();
    let code = host.start_pairing().unwrap();

    let paired = pair(Some(&address), &code, &key, info("Client"), None, None)
        .await
        .unwrap();

    (paired, Arc::new(key))
}

fn remote(paired: PairedHost, key: Arc<DeviceKey>) -> Arc<RemoteHost> {
    RemoteHost::new(paired, key, "0.0.0".into(), Retry::Forever, |_| {})
}

/// Read until `marker` shows up, reporting whether a stream reset came
/// along the way.
async fn read_until(pty: &mut impl AsyncPty, marker: &str) -> bool {
    let mut seen = Vec::new();
    let mut buf = [0; 4096];
    let mut restarted = false;

    timeout(WAIT, async {
        while !String::from_utf8_lossy(&seen).contains(marker) {
            let read = poll_fn(|cx| pty.poll_read(cx, &mut buf)).await.unwrap();

            assert_ne!(read, 0, "the terminal ended before {marker:?}");

            restarted |= pty.take_stream_reset();

            seen.extend_from_slice(&buf[..read]);
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{marker:?} never arrived"));

    restarted
}

async fn run_marker(pty: &mut impl AsyncPty, tag: &str) {
    let (command, output) = marker_command(tag);

    poll_fn(|cx| pty.poll_write(cx, &command)).await.unwrap();

    read_until(pty, &output).await;
}

async fn wait_status(host: &RemoteHost, wanted: Status) {
    let mut status = host.status();

    timeout(WAIT, status.wait_for(|status| *status == wanted))
        .await
        .unwrap_or_else(|_| panic!("status never became {wanted:?}"))
        .unwrap();
}

#[test]
fn paired_client_runs_a_command_in_a_host_terminal() {
    let host_dir = tempfile::tempdir().unwrap();
    let host = start_host(&host_dir, SessionRegistry::new());

    runtime().block_on(async {
        let (paired, key) = paired_client(&host).await;

        assert_eq!(host.devices().len(), 1);
        assert!(host.pairing().is_none(), "a used code is gone");

        let remote = remote(paired, key);

        let mut pty = remote.open_terminal(80, 24).await.unwrap();

        // The first bytes are the attach checkpoint, a stream reset.
        assert!(read_until(&mut pty, "\x1bc").await);

        run_marker(&mut pty, "FIRST").await;

        assert_eq!(host.terminal_count(), 1);

        // Closing a view detaches; ending the session is explicit.
        let session = pty.session().to_owned();

        drop(pty);
        sleep(Duration::from_millis(200)).await;

        assert_eq!(host.terminal_count(), 1);

        remote.terminate(&session);

        timeout(WAIT, async {
            while host.terminal_count() != 0 {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the host terminal closes when terminated");
    });
}

#[test]
fn a_view_survives_a_dropped_link_and_resumes_from_a_checkpoint() {
    let host_dir = tempfile::tempdir().unwrap();
    let host = start_host(&host_dir, SessionRegistry::new());

    runtime().block_on(async {
        let (paired, key) = paired_client(&host).await;
        let remote = remote(paired, key);

        let mut pty = remote.open_terminal(80, 24).await.unwrap();

        run_marker(&mut pty, "BEFORE").await;

        host.drop_connections();

        wait_status(&remote, Status::Reconnecting).await;
        wait_status(&remote, Status::Connected).await;

        // Output the old link delivered may still be queued; skip to the
        // replay, which restarts the stream and still shows the earlier
        // command because the session kept running.
        let mut buf = [0; 4096];

        timeout(WAIT, async {
            loop {
                poll_fn(|cx| pty.poll_read(cx, &mut buf)).await.unwrap();

                if pty.take_stream_reset() {
                    break;
                }
            }
        })
        .await
        .expect("the view restarts from a checkpoint");

        let replay = String::from_utf8_lossy(&buf);

        assert!(replay.contains("\x1bc"));

        // Input sent before the reattach completes would be dropped, which
        // the replay above rules out.
        let (command, output) = marker_command("AFTER");

        poll_fn(|cx| pty.poll_write(cx, &command)).await.unwrap();

        read_until(&mut pty, &output).await;
    });
}

/// The terminal session of a host tab, which the host owns and registers.
fn host_tab_session() -> SessionHandles {
    let (shell, args) = test_shell();

    start_session(
        create_pty_with_env(PtyOptions {
            shell: shell.as_deref().unwrap(),
            args: &args,
            working_directory: None,
            columns: 80,
            rows: 24,
            environment_overrides: &[],
            starting_title: None,
            bootstrap: None,
        })
        .unwrap(),
        VoidListener,
        SessionOptions {
            cols: 80,
            rows: 24,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: CursorShape::Block,
            scrollback_lines: 1000,
            engine_blocks: false,
            terminal_responses: true,
        },
    )
    .unwrap()
}

#[test]
fn host_tabs_are_listed_attachable_and_not_closable_remotely() {
    let host_dir = tempfile::tempdir().unwrap();
    let registry = SessionRegistry::new();
    let host = start_host(&host_dir, Arc::clone(&registry));

    // A host tab: the host owns the session and registers it.
    let tab = host_tab_session();

    let session = registry.register_tab(
        "Tab".into(),
        80,
        24,
        TerminalControl {
            messenger: tab.messenger.clone(),
            claimed_remotely: Arc::new(AtomicBool::new(false)),
        },
    );

    let (paired, key) = runtime().block_on(paired_client(&host));
    let remote = remote(paired, key);

    // Tabs open views from the UI thread, outside any runtime context.
    let mut pty = remote.view(session.clone());

    runtime().block_on(async {
        let sessions = remote.list_sessions().await.unwrap();

        assert!(
            sessions
                .iter()
                .any(|info| info.session == session && info.origin == Origin::Tab)
        );

        run_marker(&mut pty, "TAB").await;

        remote.terminate(&session);

        sleep(Duration::from_millis(300)).await;

        assert!(registry.list().iter().any(|info| info.session == session));
    });

    registry.unregister(&session);
}

#[test]
fn a_device_attaching_to_a_pending_host_tab_starts_it_under_its_listed_id() {
    let host_dir = tempfile::tempdir().unwrap();
    let registry = SessionRegistry::new();
    let host = start_host(&host_dir, Arc::clone(&registry));

    let session = registry.register_pending_tab("Pending".into());

    let (requests, mut requests_rx) = mpsc::unbounded_channel();

    registry.serve_host_requests(requests);

    // The application starts the tab's shell when asked, as activating the
    // tab on the host would, and offers it under the id it was listed by.
    let started = Arc::clone(&registry);

    runtime().spawn(async move {
        let mut tabs = Vec::new();

        while let Some(request) = requests_rx.recv().await {
            if let HostRequest::StartTab { session, reply } = request {
                let tab = host_tab_session();

                started.register_tab_as(
                    session,
                    "Started".into(),
                    80,
                    24,
                    TerminalControl {
                        messenger: tab.messenger.clone(),
                        claimed_remotely: Arc::new(AtomicBool::new(false)),
                    },
                );

                tabs.push(tab);

                let _ = reply.send(Ok(()));
            }
        }
    });

    let (paired, key) = runtime().block_on(paired_client(&host));
    let remote = remote(paired, key);

    runtime().block_on(async {
        let sessions = remote.list_sessions().await.unwrap();

        // The device sees the tab as asleep, which its sidebar folds by.
        assert!(sessions.iter().any(|info| info.session == session
            && info.origin == Origin::Tab
            && info.kind == SessionKind::Terminal
            && info.pending));
    });

    let mut pty = remote.view(session.clone());

    runtime().block_on(async {
        run_marker(&mut pty, "PENDING").await;

        assert!(!registry.is_pending(&session));

        let listed: Vec<_> = registry
            .list()
            .into_iter()
            .filter(|info| info.session == session)
            .collect();

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "Started");
        assert!(!listed[0].pending);
    });

    registry.unregister(&session);
}

#[test]
fn terminal_ids_of_a_restarted_host_never_repeat_the_previous_run() {
    let before = SessionRegistry::new().register_pending_tab("Tab".into());
    let after = SessionRegistry::new().register_pending_tab("Tab".into());

    // Both runs list their first tab, which once shared the id `t1`; a
    // device still holding the old id must not start the new run's tab.
    assert_ne!(before, after);
}

#[test]
fn a_view_ends_when_its_shell_exits() {
    let host_dir = tempfile::tempdir().unwrap();
    let host = start_host(&host_dir, SessionRegistry::new());

    runtime().block_on(async {
        let (paired, key) = paired_client(&host).await;
        let remote = remote(paired, key);

        let mut pty = remote.open_terminal(80, 24).await.unwrap();

        run_marker(&mut pty, "ALIVE").await;

        poll_fn(|cx| pty.poll_write(cx, b"exit\r")).await.unwrap();

        let mut buf = [0; 4096];

        timeout(WAIT, async {
            loop {
                let read = poll_fn(|cx| pty.poll_read(cx, &mut buf)).await.unwrap();

                if read == 0 {
                    break;
                }
            }
        })
        .await
        .expect("the view ends with its shell");

        assert!(poll_fn(|cx| Poll::Ready(pty.poll_exit(cx).is_ready())).await);
    });
}

#[test]
fn unpaired_and_revoked_devices_are_refused() {
    let host_dir = tempfile::tempdir().unwrap();
    let host = start_host(&host_dir, SessionRegistry::new());
    let address = format!("127.0.0.1:{}", host.local_addr().port());

    runtime().block_on(async {
        let wrong = PairingCode::generate().unwrap();

        host.start_pairing().unwrap();

        assert!(
            pair(
                Some(&address),
                &wrong,
                &DeviceKey::generate().unwrap(),
                info("Guess"),
                None,
                None
            )
            .await
            .is_err()
        );

        let (paired, key) = paired_client(&host).await;

        let stranger = remote(paired.clone(), Arc::new(DeviceKey::generate().unwrap()));

        assert!(stranger.list_sessions().await.is_err());

        wait_status(&stranger, Status::Refused).await;

        let client = remote(paired, Arc::clone(&key));

        client.list_sessions().await.unwrap();

        host.remove_device(&key.id()).unwrap();

        // With nothing open the link idles once the host drops it, so the
        // revocation shows on the next request that connects. One sent
        // before the client noticed the drop fails on the old link instead.
        let _ = client.list_sessions().await;

        assert!(client.list_sessions().await.is_err());

        wait_status(&client, Status::Refused).await;
    });
}

#[test]
fn only_a_timed_out_pairing_code_is_renewed() {
    let host_dir = tempfile::tempdir().unwrap();
    let host = start_host(&host_dir, SessionRegistry::new());

    let first = host.start_pairing().unwrap();

    assert!(!host.renew_expired_pairing().unwrap());

    host.expire_pairing();

    assert!(host.pairing().is_none());
    assert!(host.renew_expired_pairing().unwrap());

    let (renewed, _) = host.pairing().expect("a fresh code is showing");

    assert_ne!(renewed, first);

    host.cancel_pairing();

    assert!(!host.renew_expired_pairing().unwrap());
    assert!(host.pairing().is_none());
}

/// Needs a network interface that supports multicast, which CI runners and
/// some VPNs lack.
#[test]
#[ignore = "needs LAN multicast"]
fn pairing_without_an_address_finds_the_host_showing_the_code() {
    let host_dir = tempfile::tempdir().unwrap();
    let host = start_host(&host_dir, SessionRegistry::new());
    let key = DeviceKey::generate().unwrap();
    let code = host.start_pairing().unwrap();

    runtime().block_on(async {
        let mut paired = pair(None, &code, &key, info("Client"), None, None)
            .await
            .unwrap();

        assert_eq!(paired.id, host.device_id());

        // A stale address falls back to finding the host by its id.
        paired.lan_hints = vec!["127.0.0.1:1".into()];

        let client = remote(paired, Arc::new(key));

        client.list_sessions().await.unwrap();
    });
}

/// Needs a running relay: `wrangler dev` under `relay/`, with
/// `NMT_TEST_RELAY_URL` and `NMT_TEST_RELAY_KEY` pointing at it.
#[test]
#[ignore = "needs a running relay"]
fn a_host_off_the_lan_is_paired_and_used_through_the_relay() {
    let relay = RelayAccess {
        url: env::var("NMT_TEST_RELAY_URL").expect("NMT_TEST_RELAY_URL"),
        access_key: env::var("NMT_TEST_RELAY_KEY").expect("NMT_TEST_RELAY_KEY"),
    };

    let host_dir = tempfile::tempdir().unwrap();
    let host = start_host_with_relay(&host_dir, SessionRegistry::new(), Some(relay.clone()));

    host.close_lan();

    let key = DeviceKey::generate().unwrap();
    let code = host.start_pairing().unwrap();

    runtime().block_on(async {
        // The host registers and claims the code's slot in the background.
        let deadline = Instant::now() + WAIT;

        let paired = loop {
            match pair(None, &code, &key, info("Client"), None, Some(relay.clone())).await {
                Ok(paired) => break paired,
                Err(error) if Instant::now() < deadline => {
                    eprintln!("retrying pairing: {error:#}");

                    sleep(Duration::from_millis(250)).await;
                }
                Err(error) => panic!("pairing through the relay failed: {error:#}"),
            }
        };

        assert_eq!(paired.relay.as_ref().unwrap().open().unwrap(), relay);

        // The relay won, so no address that worked leads the list: it is
        // exactly what the host said about itself, for when this device is
        // on its network. Its LAN listener is closed here, so trying those
        // addresses first must not keep the relay from serving the session.
        let port = host.local_addr().port();

        let mut advertised: Vec<String> = lan_addresses()
            .into_iter()
            .map(|ip| format!("{ip}:{port}"))
            .collect();

        advertised.truncate(4);

        assert_eq!(paired.lan_hints, advertised);

        let remote = remote(paired, Arc::new(key));

        let mut pty = remote.open_terminal(80, 24).await.unwrap();

        run_marker(&mut pty, "RELAYED").await;
    });
}

/// Needs a running relay, as the test above. A device paired through the
/// relay knows the host's LAN addresses and connects over the LAN next time.
#[test]
#[ignore = "needs a running relay"]
fn a_device_paired_through_the_relay_goes_direct_on_the_hosts_lan() {
    if lan_addresses().is_empty() {
        eprintln!("skipped: this machine has no LAN address");

        return;
    }

    let relay = RelayAccess {
        url: env::var("NMT_TEST_RELAY_URL").expect("NMT_TEST_RELAY_URL"),
        access_key: env::var("NMT_TEST_RELAY_KEY").expect("NMT_TEST_RELAY_KEY"),
    };

    let host_dir = tempfile::tempdir().unwrap();
    let host = start_host_with_relay(&host_dir, SessionRegistry::new(), Some(relay.clone()));

    let key = DeviceKey::generate().unwrap();
    let code = host.start_pairing().unwrap();

    runtime().block_on(async {
        let deadline = Instant::now() + WAIT;

        // By pairing slot, with no address: through the relay, unless DNS-SD
        // finds the host first.
        let mut paired = loop {
            match pair(None, &code, &key, info("Client"), None, Some(relay.clone())).await {
                Ok(paired) => break paired,
                Err(error) if Instant::now() < deadline => {
                    eprintln!("retrying pairing: {error:#}");

                    sleep(Duration::from_millis(250)).await;
                }
                Err(error) => panic!("pairing through the relay failed: {error:#}"),
            }
        };

        // Pairing through the relay handed over the host's LAN addresses.
        assert!(!paired.lan_hints.is_empty());

        // Without the relay, the next connection can only go direct, to an
        // address learned at pairing.
        paired.relay = None;

        let client = remote(paired, Arc::new(key));

        client.list_sessions().await.unwrap();
    });
}

/// Needs a running relay, as the tests above. A link that came up through the
/// relay moves to the LAN once the host answers there, and its terminal view
/// continues over the new link.
#[test]
#[ignore = "needs a running relay"]
fn a_link_through_the_relay_moves_to_the_lan_once_the_host_answers_there() {
    if lan_addresses().is_empty() {
        eprintln!("skipped: this machine has no LAN address");

        return;
    }

    let relay = RelayAccess {
        url: env::var("NMT_TEST_RELAY_URL").expect("NMT_TEST_RELAY_URL"),
        access_key: env::var("NMT_TEST_RELAY_KEY").expect("NMT_TEST_RELAY_KEY"),
    };

    let host_dir = tempfile::tempdir().unwrap();
    let host = start_host_with_relay(&host_dir, SessionRegistry::new(), Some(relay.clone()));

    // As on a network that keeps devices apart: everything goes through the
    // relay, even when DNS-SD finds the host.
    host.pause_lan(true);

    let key = DeviceKey::generate().unwrap();
    let code = host.start_pairing().unwrap();

    runtime().block_on(async {
        let deadline = Instant::now() + WAIT;

        let paired = loop {
            match pair(None, &code, &key, info("Client"), None, Some(relay.clone())).await {
                Ok(paired) => break paired,
                Err(error) if Instant::now() < deadline => {
                    eprintln!("retrying pairing: {error:#}");

                    sleep(Duration::from_millis(250)).await;
                }
                Err(error) => panic!("pairing through the relay failed: {error:#}"),
            }
        };

        let client = remote(paired, Arc::new(key));

        let mut pty = client.open_terminal(80, 24).await.unwrap();

        run_marker(&mut pty, "RELAYED").await;

        assert_eq!(client.path(), Some(LinkPath::Relay));

        // The device reaches the host's network.
        host.pause_lan(false);

        notify_network_changed();

        timeout(WAIT, async {
            while !matches!(client.path(), Some(LinkPath::Lan(_))) {
                sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the link stayed on {:?}", client.path()));

        run_marker(&mut pty, "DIRECT").await;
    });
}

/// Needs a running relay, as the tests above. A link follows the path
/// policy: pinned to the relay it leaves the LAN, pinned to the LAN it comes
/// back, and its terminal view continues across both moves.
#[test]
#[ignore = "needs a running relay"]
fn a_link_moves_to_the_path_its_policy_allows() {
    if lan_addresses().is_empty() {
        eprintln!("skipped: this machine has no LAN address");

        return;
    }

    let relay = RelayAccess {
        url: env::var("NMT_TEST_RELAY_URL").expect("NMT_TEST_RELAY_URL"),
        access_key: env::var("NMT_TEST_RELAY_KEY").expect("NMT_TEST_RELAY_KEY"),
    };

    let host_dir = tempfile::tempdir().unwrap();
    let host = start_host_with_relay(&host_dir, SessionRegistry::new(), Some(relay.clone()));

    let key = DeviceKey::generate().unwrap();
    let code = host.start_pairing().unwrap();

    runtime().block_on(async {
        let deadline = Instant::now() + WAIT;

        let paired = loop {
            match pair(None, &code, &key, info("Client"), None, Some(relay.clone())).await {
                Ok(paired) => break paired,
                Err(error) if Instant::now() < deadline => {
                    eprintln!("retrying pairing: {error:#}");

                    sleep(Duration::from_millis(250)).await;
                }
                Err(error) => panic!("pairing through the relay failed: {error:#}"),
            }
        };

        let client = remote(paired, Arc::new(key));

        // Input sent before the view reattaches on the new link is dropped,
        // so each move waits for the view to restart from its checkpoint.
        let wait_moved = async |pty: &mut NetworkPty, relayed: bool| {
            let mut buf = [0; 4096];

            timeout(WAIT, async {
                loop {
                    poll_fn(|cx| pty.poll_read(cx, &mut buf)).await.unwrap();

                    if pty.take_stream_reset() {
                        break;
                    }
                }
            })
            .await
            .expect("the view restarts from a checkpoint on the new link");

            assert_eq!(
                client.path().map(|path| path == LinkPath::Relay),
                Some(relayed)
            );
        };

        client.set_path_policy(PathPolicy::Relay);

        let mut pty = client.open_terminal(80, 24).await.unwrap();

        run_marker(&mut pty, "PINNED_RELAY").await;

        assert_eq!(client.path(), Some(LinkPath::Relay));

        client.set_path_policy(PathPolicy::Lan);

        wait_moved(&mut pty, false).await;

        run_marker(&mut pty, "PINNED_LAN").await;

        client.set_path_policy(PathPolicy::Relay);

        wait_moved(&mut pty, true).await;

        run_marker(&mut pty, "BACK_ON_RELAY").await;
    });
}

/// An agent session stand-in: every view gets a snapshot numbered by how
/// many views attached so far and then one change, and calls echo.
fn fake_agent(registry: &SessionRegistry) -> String {
    let (requests, mut requests_rx) = mpsc::unbounded_channel();

    runtime().spawn(async move {
        let mut attached = 0;

        while let Some(request) = requests_rx.recv().await {
            match request {
                AgentRequest::Attach { updates, reply } => {
                    attached += 1;

                    let _ = reply.send(json!({ "snapshot": attached }));
                    let _ = updates.send(json!({ "change": attached }));
                }
                AgentRequest::AttachPaged { reply, .. } => {
                    drop(reply);
                }
                AgentRequest::Page { reply, .. } => {
                    let _ = reply.send(Err("this test agent does not support paged views".into()));
                }
                AgentRequest::Call {
                    method,
                    params,
                    reply,
                } => {
                    let _ = reply.send(Ok(json!({ "method": method, "params": params })));
                }
            }
        }
    });

    let session = String::from("a-test");

    registry.register_agent(
        session.clone(),
        "Agent".into(),
        "Claude".into(),
        AgentControl { requests },
    );

    session
}

async fn next_update(updates: &mut UnboundedReceiver<AgentUpdate>) -> AgentUpdate {
    timeout(WAIT, updates.recv())
        .await
        .expect("an agent update arrives")
        .expect("the agent view stays open")
}

/// Synthetic payloads exercise the real encrypted transport without reading conversations.
fn fake_paged_agent(
    registry: &SessionRegistry,
    snapshot_bytes: usize,
    update_bytes: usize,
) -> String {
    let (requests, mut requests_rx) = mpsc::unbounded_channel();

    runtime().spawn(async move {
        let mut attached = 0;
        let mut views = HashMap::new();

        while let Some(request) = requests_rx.recv().await {
            match request {
                AgentRequest::AttachPaged { view_id, updates, reply } => {
                    attached += 1;

                    let _ = reply.send(json!({
                        "view_id": view_id, "type": "snapshot",
                        "value": { "attach": attached, "body": "x".repeat(snapshot_bytes) },
                    }));

                    let _ = updates.send(json!({
                        "view_id": view_id, "type": "update",
                        "value": { "sequence": 1, "body": "y".repeat(update_bytes) },
                    })).await;

                    views.insert(view_id, updates);
                }
                AgentRequest::Page { view_id, cursor, reply } => {
                    let outcome = if let Some(updates) = views.get(&view_id) {
                        updates.send(json!({
                            "view_id": view_id, "type": "page",
                            "value": { "requested": cursor, "entries": ["earlier synthetic entry"] },
                        })).await.map(|()| json!({})).map_err(|_| "view closed".to_owned())
                    } else {
                        Err("view not attached".to_owned())
                    };

                    let _ = reply.send(outcome);
                }
                AgentRequest::Attach { reply, .. } => drop(reply),
                AgentRequest::Call { reply, .. } => {
                    let _ = reply.send(Ok(json!({})));
                }
            }
        }
    });

    let session = "a-paged-test".to_owned();

    registry.register_agent(
        session.clone(),
        "Paged agent".into(),
        "Test".into(),
        AgentControl { requests },
    );

    session
}

async fn next_paged_update(updates: &mut UnboundedReceiver<AgentUpdate>) -> Value {
    match next_update(updates).await {
        AgentUpdate::Paged(value) => value,
        _ => panic!("expected a paged event"),
    }
}

#[test]
fn paged_agent_large_snapshot_and_update_keep_order_and_load_an_earlier_page() {
    let host_dir = tempfile::tempdir().unwrap();
    let registry = SessionRegistry::new();
    let host = start_host(&host_dir, Arc::clone(&registry));
    let bytes = 9 * 1024 * 1024;
    let session = fake_paged_agent(&registry, bytes, bytes);

    runtime().block_on(async {
        let (paired, key) = paired_client(&host).await;
        let remote = remote(paired, key);

        let (link, mut updates) = remote.agent_view_paged(session.clone());

        let snapshot = next_paged_update(&mut updates).await;

        assert_eq!(snapshot["type"], "snapshot");
        assert_eq!(snapshot["value"]["body"].as_str().unwrap().len(), bytes);

        let view_id = snapshot["view_id"].as_str().unwrap().to_owned();

        drop(snapshot);

        let update = next_paged_update(&mut updates).await;

        assert_eq!(update["type"], "update");
        assert_eq!(update["value"]["body"].as_str().unwrap().len(), bytes);

        drop(update);

        let cursor = json!({ "start": 40, "revision": 2 });

        link.load_earlier(cursor.clone()).await.unwrap();

        let page = next_paged_update(&mut updates).await;

        assert_eq!(page["type"], "page");
        assert_eq!(page["view_id"], view_id);
        assert_eq!(page["value"]["requested"], cursor);
        assert_eq!(page["value"]["entries"].as_array().unwrap().len(), 1);
        assert!(
            remote
                .list_sessions()
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.session == session)
        );

        let error = remote
            .call::<Value>(
                rpc::AGENT_PAGE,
                &json!({
                    "session": session, "view_id": "stale-view", "cursor": {},
                }),
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("no longer attached"));

        drop(link);
    });
}

#[test]
fn paged_agent_reattaches_with_fresh_view_ids_after_a_drop_and_detach() {
    let host_dir = tempfile::tempdir().unwrap();
    let registry = SessionRegistry::new();
    let host = start_host(&host_dir, Arc::clone(&registry));
    let session = fake_paged_agent(&registry, 1, 1);

    runtime().block_on(async {
        let (paired, key) = paired_client(&host).await;
        let remote = remote(paired, key);

        let (link, mut updates) = remote.agent_view_paged(session.clone());

        let first = next_paged_update(&mut updates).await;
        let first_id = first["view_id"].as_str().unwrap().to_owned();

        assert_eq!(first["type"], "snapshot");
        assert_eq!(next_paged_update(&mut updates).await["type"], "update");

        host.drop_connections();

        let second = next_paged_update(&mut updates).await;
        let second_id = second["view_id"].as_str().unwrap().to_owned();

        assert_eq!(second["type"], "snapshot");
        assert_ne!(first_id, second_id);
        assert_eq!(second["value"]["attach"], 2);
        assert_eq!(next_paged_update(&mut updates).await["type"], "update");

        let error = remote
            .call::<Value>(
                rpc::AGENT_PAGE,
                &json!({
                    "session": session, "view_id": first_id, "cursor": {},
                }),
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("no longer attached"));

        drop(link);

        timeout(WAIT, async {
            while !registry.viewers(&session).is_empty() {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();

        let (reopened, mut reopened_updates) = remote.agent_view_paged(session.clone());

        let third = next_paged_update(&mut reopened_updates).await;

        assert_eq!(third["type"], "snapshot");
        assert_ne!(third["view_id"].as_str().unwrap(), second_id);
        assert_eq!(third["value"]["attach"], 3);

        drop(reopened);
    });
}

#[test]
fn paged_agent_oversized_snapshot_is_an_explicit_error_without_closing_the_host_link() {
    let host_dir = tempfile::tempdir().unwrap();
    let registry = SessionRegistry::new();
    let host = start_host(&host_dir, Arc::clone(&registry));
    let session = fake_paged_agent(&registry, MAX_CONTROL_TRANSFER, 0);

    runtime().block_on(async {
        let (paired, key) = paired_client(&host).await;
        let remote = remote(paired, key);

        let (link, mut updates) = remote.agent_view_paged(session.clone());

        match next_update(&mut updates).await {
            AgentUpdate::Error(message) => assert!(message.contains("64 MiB")),
            _ => panic!("expected an explicit oversized snapshot error"),
        }

        assert!(
            remote
                .list_sessions()
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.session == session)
        );

        drop(link);
    });
}

#[test]
fn paged_agent_oversized_live_update_is_an_explicit_error_without_closing_the_host_link() {
    let host_dir = tempfile::tempdir().unwrap();
    let registry = SessionRegistry::new();
    let host = start_host(&host_dir, Arc::clone(&registry));
    let session = fake_paged_agent(&registry, 1, MAX_CONTROL_TRANSFER);

    runtime().block_on(async {
        let (paired, key) = paired_client(&host).await;
        let remote = remote(paired, key);

        let (link, mut updates) = remote.agent_view_paged(session.clone());

        assert_eq!(next_paged_update(&mut updates).await["type"], "snapshot");

        let error = next_paged_update(&mut updates).await;

        assert_eq!(error["type"], "error");
        assert!(
            error["value"]["message"]
                .as_str()
                .unwrap()
                .contains("64 MiB")
        );
        assert!(
            remote
                .list_sessions()
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.session == session)
        );

        drop(link);
    });
}

#[test]
fn an_agent_view_gets_a_snapshot_then_changes_and_again_after_a_drop() {
    let host_dir = tempfile::tempdir().unwrap();
    let registry = SessionRegistry::new();
    let host = start_host(&host_dir, Arc::clone(&registry));
    let session = fake_agent(&registry);

    runtime().block_on(async {
        let (paired, key) = paired_client(&host).await;
        let remote = remote(paired, key);

        let sessions = remote.list_sessions().await.unwrap();

        assert!(sessions.iter().any(|info| info.session == session
            && info.kind == SessionKind::Agent
            && info.harness.as_deref() == Some("Claude")));

        let (link, mut updates) = remote.agent_view(session.clone());

        assert!(matches!(
            next_update(&mut updates).await,
            AgentUpdate::Snapshot(view) if view == json!({ "snapshot": 1 })
        ));
        assert!(matches!(
            next_update(&mut updates).await,
            AgentUpdate::Ops(ops) if ops == json!({ "change": 1 })
        ));

        let echoed = link.call("interrupt", json!({ "a": 1 })).await.unwrap();

        assert_eq!(
            echoed,
            json!({ "method": "interrupt", "params": { "a": 1 } })
        );

        host.drop_connections();

        // The view reattaches by itself and starts over from a snapshot.
        assert!(matches!(
            next_update(&mut updates).await,
            AgentUpdate::Snapshot(view) if view == json!({ "snapshot": 2 })
        ));

        registry.unregister(&session);

        drop(link);
    });
}

#[test]
fn a_host_view_of_a_remote_created_terminal_shares_it_with_the_device() {
    let host_dir = tempfile::tempdir().unwrap();
    let registry = SessionRegistry::new();
    let host = start_host(&host_dir, Arc::clone(&registry));

    runtime().block_on(async {
        let (paired, key) = paired_client(&host).await;
        let remote = remote(paired, key);

        let mut device = remote.open_terminal(80, 24).await.unwrap();

        let session = device.session().to_owned();

        assert!(read_until(&mut device, "c").await);

        assert_eq!(host.connected_devices(), vec![String::from("Client")]);
        assert_eq!(registry.viewers(&session), vec![String::from("Client")]);

        let mut local = local_view::open(&registry, &session).expect("the terminal is running");

        // The host view starts from a checkpoint, like a remote one.
        assert!(read_until(&mut local, "c").await);

        // What the host types reaches the device, which sees one terminal.
        let (command, output) = marker_command("LOCAL");

        poll_fn(|cx| local.poll_write(cx, &command)).await.unwrap();

        read_until(&mut local, &output).await;
        read_until(&mut device, &output).await;

        // Closing the host view leaves the terminal running for the device.
        drop(local);

        run_marker(&mut device, "STILL").await;

        drop(device);

        timeout(WAIT, async {
            while !registry.viewers(&session).is_empty() {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a closed view stops listing its device");

        assert_eq!(host.terminal_count(), 1);
    });
}

#[test]
fn a_device_lists_what_it_may_start_and_opens_tabs_on_the_host() {
    let host_dir = tempfile::tempdir().unwrap();
    let registry = SessionRegistry::new();
    let host = start_host(&host_dir, Arc::clone(&registry));

    runtime().block_on(async {
        let (paired, key) = paired_client(&host).await;
        let remote = remote(paired, key);

        // Before the application answers host requests, a device hears that
        // the host cannot start anything.
        assert!(remote.host_info().await.is_err());

        let (requests, mut requests_rx) = mpsc::unbounded_channel();

        registry.serve_host_requests(requests);

        let opened = Arc::clone(&registry);

        runtime().spawn(async move {
            while let Some(request) = requests_rx.recv().await {
                match request {
                    HostRequest::Info { reply } => {
                        let _ = reply.send(Ok(json!({
                            "agents": [{ "name": "Codex", "harness": "codex" }],
                            "workspaces": [{ "name": "work", "path": "C:/work" }],
                        })));
                    }
                    HostRequest::OpenAgent { params, reply } => {
                        if params["workspace"] != "C:/work" {
                            let _ = reply.send(Err("not a host workspace".into()));

                            continue;
                        }

                        let session = fake_agent(&opened);

                        let _ = reply.send(Ok(json!({ "session": session })));
                    }
                    HostRequest::OpenTerminalTab { params, reply } => {
                        let _ = reply.send(if params["workspace"] == "C:/work" {
                            Ok(json!({ "session": format!("work-{}", params["profile"]) }))
                        } else {
                            Err("not a host workspace".into())
                        });
                    }
                    HostRequest::CloseSession { reply, .. } => {
                        let _ = reply.send(Err("not closed in this test".into()));
                    }
                    HostRequest::StartTab { reply, .. } => {
                        let _ = reply.send(Err("not started in this test".into()));
                    }
                    HostRequest::RenameSession { reply, .. }
                    | HostRequest::RenameHost { reply, .. } => {
                        let _ = reply.send(Err("not renamed in this test".into()));
                    }
                }
            }
        });

        let info = remote.host_info().await.unwrap();

        assert_eq!(info.agents[0].harness, "codex");
        assert_eq!(info.workspaces[0].path, "C:/work");

        // Only a listed workspace is accepted.
        assert!(
            remote
                .open_agent("Codex".into(), "C:/elsewhere".into())
                .await
                .is_err()
        );

        // A terminal tab goes through the application too, and only into a
        // listed workspace; the profile reaches it, and an omitted one
        // arrives as absent.
        assert!(
            remote
                .open_terminal_tab("C:/elsewhere".into(), None)
                .await
                .is_err()
        );

        assert_eq!(
            remote
                .open_terminal_tab("C:/work".into(), Some("pwsh".into()))
                .await
                .unwrap(),
            "work-\"pwsh\""
        );

        assert_eq!(
            remote
                .open_terminal_tab("C:/work".into(), None)
                .await
                .unwrap(),
            "work-null"
        );

        let session = remote
            .open_agent("Codex".into(), "C:/work".into())
            .await
            .unwrap();

        let sessions = remote.list_sessions().await.unwrap();

        assert!(sessions.iter().any(|info| info.session == session));

        let (_link, mut updates) = remote.agent_view(session);

        assert!(matches!(
            next_update(&mut updates).await,
            AgentUpdate::Snapshot(_)
        ));
    });
}

#[test]
fn a_device_closes_host_tabs_through_the_application_and_its_own_terminals_directly() {
    let host_dir = tempfile::tempdir().unwrap();
    let registry = SessionRegistry::new();
    let host = start_host(&host_dir, Arc::clone(&registry));

    runtime().block_on(async {
        let (paired, key) = paired_client(&host).await;
        let remote = remote(paired, key);

        let agent = fake_agent(&registry);

        // Only the application can close a host tab, so before it answers
        // host requests the host refuses and the tab stays.
        assert!(remote.close_session(agent.clone()).await.is_err());
        assert!(registry.list().iter().any(|info| info.session == agent));

        let (requests, mut requests_rx) = mpsc::unbounded_channel();

        registry.serve_host_requests(requests);

        let closing = Arc::clone(&registry);

        let (asked, mut asked_rx) = mpsc::unbounded_channel();

        runtime().spawn(async move {
            while let Some(request) = requests_rx.recv().await {
                if let HostRequest::CloseSession { session, reply } = request {
                    let _ = asked.send(session.clone());

                    closing.unregister(&session);

                    let _ = reply.send(Ok(()));
                }
            }
        });

        remote.close_session(agent.clone()).await.unwrap();

        assert_eq!(asked_rx.recv().await.as_deref(), Some(agent.as_str()));

        let sessions = remote.list_sessions().await.unwrap();

        assert!(!sessions.iter().any(|info| info.session == agent));

        // A session the host does not list is refused without asking the
        // application.
        assert!(remote.close_session("t-missing".into()).await.is_err());

        // A terminal a device started ends on the host without asking the
        // application, and its views hear that it was closed.
        let pty = remote.open_terminal(80, 24).await.unwrap();
        let terminal = pty.session().to_owned();

        remote.close_session(terminal.clone()).await.unwrap();

        wait_ended(&remote, &terminal, Some(EndReason::Closed)).await;

        assert!(asked_rx.try_recv().is_err());
        assert_eq!(host.terminal_count(), 0);
    });
}

#[test]
fn a_device_renames_host_tabs_through_the_application_and_its_own_terminals_directly() {
    let host_dir = tempfile::tempdir().unwrap();
    let registry = SessionRegistry::new();
    let host = start_host(&host_dir, Arc::clone(&registry));

    runtime().block_on(async {
        let (paired, key) = paired_client(&host).await;
        let remote = remote(paired, key);

        let agent = fake_agent(&registry);

        let (requests, mut requests_rx) = mpsc::unbounded_channel();

        registry.serve_host_requests(requests);

        // The application renames the tab, which lists it under the new name.
        let renaming = Arc::clone(&registry);

        let (asked, mut asked_rx) = mpsc::unbounded_channel();

        runtime().spawn(async move {
            while let Some(request) = requests_rx.recv().await {
                if let HostRequest::RenameSession {
                    session,
                    title,
                    reply,
                } = request
                {
                    let _ = asked.send(session.clone());

                    renaming.set_title(&session, title);

                    let _ = reply.send(Ok(()));
                }
            }
        });

        remote
            .rename_session(agent.clone(), "  Review  ".into())
            .await
            .unwrap();

        assert_eq!(asked_rx.recv().await.as_deref(), Some(agent.as_str()));

        let listed = |sessions: &[SessionInfo], id: &str| {
            sessions
                .iter()
                .find(|info| info.session == id)
                .map(|info| info.title.clone())
        };

        let sessions = remote.list_sessions().await.unwrap();

        // The host trims the name before anything takes it.
        assert_eq!(listed(&sessions, &agent).as_deref(), Some("Review"));

        // An empty name and a session the host does not list are refused
        // without asking the application.
        assert!(
            remote
                .rename_session(agent.clone(), " ".into())
                .await
                .is_err()
        );
        assert!(
            remote
                .rename_session("t-missing".into(), "Name".into())
                .await
                .is_err()
        );

        // A terminal a device started takes the name on the host directly.
        let pty = remote.open_terminal(80, 24).await.unwrap();
        let terminal = pty.session().to_owned();

        remote
            .rename_session(terminal.clone(), "Build".into())
            .await
            .unwrap();

        let sessions = remote.list_sessions().await.unwrap();

        assert_eq!(listed(&sessions, &terminal).as_deref(), Some("Build"));
        assert!(asked_rx.try_recv().is_err());
    });
}

/// Collect each host record a connection stores.
fn recording_remote(
    paired: PairedHost,
    key: Arc<DeviceKey>,
) -> (Arc<RemoteHost>, UnboundedReceiver<PairedHost>) {
    let (records, records_rx) = mpsc::unbounded_channel();

    let remote = RemoteHost::new(paired, key, "0.0.0".into(), Retry::Forever, move |record| {
        let _ = records.send(record);
    });

    (remote, records_rx)
}

async fn wait_named(records: &mut UnboundedReceiver<PairedHost>, name: &str) {
    timeout(WAIT, async {
        while records.recv().await.expect("records keep coming").name != name {}
    })
    .await
    .unwrap_or_else(|_| panic!("the host record never took the name {name}"));
}

#[test]
fn a_device_renames_the_host_and_every_device_stores_the_new_name() {
    let host_dir = tempfile::tempdir().unwrap();
    let registry = SessionRegistry::new();
    let host = Arc::new(start_host(&host_dir, Arc::clone(&registry)));

    runtime().block_on(async {
        let (renamer_record, renamer_key) = paired_client(&host).await;
        let (watcher_record, watcher_key) = paired_client(&host).await;
        let offline_record = watcher_record.clone();
        let offline_key = Arc::clone(&watcher_key);

        let (renamer, mut renamer_records) = recording_remote(renamer_record, renamer_key);
        let (watcher, mut watcher_records) = recording_remote(watcher_record, watcher_key);

        watcher.keep_connected();

        wait_named(&mut watcher_records, "Host").await;

        let (requests, mut requests_rx) = mpsc::unbounded_channel();

        registry.serve_host_requests(requests);

        // The application stores the name and hands it back to the host,
        // and the host then sends it to the devices.
        let (asked, mut asked_rx) = mpsc::unbounded_channel();

        let renamed = Arc::clone(&host);

        runtime().spawn(async move {
            while let Some(request) = requests_rx.recv().await {
                if let HostRequest::RenameHost { name, reply } = request {
                    let _ = asked.send(name.clone());

                    renamed.set_device_name(name);

                    let _ = reply.send(Ok(()));
                }
            }
        });

        renamer.rename_host("  Studio  ".into()).await.unwrap();

        // The host trims the name before the application sees it.
        assert_eq!(asked_rx.recv().await.as_deref(), Some("Studio"));

        wait_named(&mut renamer_records, "Studio").await;
        wait_named(&mut watcher_records, "Studio").await;

        // A blank name is refused without asking the application.
        assert!(renamer.rename_host(" ".into()).await.is_err());
        assert!(asked_rx.try_recv().is_err());

        // A device that was away holds the old name until its next
        // handshake brings the new one.
        drop(watcher);

        let (returning, mut returning_records) = recording_remote(offline_record, offline_key);

        returning.keep_connected();

        wait_named(&mut returning_records, "Studio").await;
    });
}

async fn wait_ended(remote: &RemoteHost, session: &str, wanted: Option<EndReason>) {
    let mut changes = remote.ended_changes();

    timeout(WAIT, async {
        while remote.ended(session) != wanted {
            let _ = changes.changed().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the view never became {wanted:?}"));
}

#[test]
fn the_host_takes_a_session_back_and_the_device_takes_it_again() {
    let host_dir = tempfile::tempdir().unwrap();
    let registry = SessionRegistry::new();
    let host = start_host(&host_dir, Arc::clone(&registry));

    runtime().block_on(async {
        let (paired, key) = paired_client(&host).await;
        let remote = remote(paired, key);

        let mut pty = remote.open_terminal(80, 24).await.unwrap();

        let session = pty.session().to_owned();

        run_marker(&mut pty, "BEFORE").await;

        registry.take_back(&session);

        // The view stays open and says why, and nobody is listed as
        // controlling the session any more.
        wait_ended(&remote, &session, Some(EndReason::TakenBack)).await;

        assert!(registry.viewers(&session).is_empty());

        remote.reconnect(&session);

        wait_ended(&remote, &session, None).await;

        // Taking it up again replays the terminal and passes input again.
        let mut buf = [0; 4096];

        timeout(WAIT, async {
            loop {
                poll_fn(|cx| pty.poll_read(cx, &mut buf)).await.unwrap();

                if pty.take_stream_reset() {
                    break;
                }
            }
        })
        .await
        .expect("the view replays from a checkpoint");

        run_marker(&mut pty, "AFTER").await;

        assert_eq!(registry.viewers(&session), vec![String::from("Client")]);

        registry.close_remote(&session);

        wait_ended(&remote, &session, Some(EndReason::Closed)).await;
    });
}

fn registration() -> PushRegistration {
    PushRegistration {
        endpoint: "https://relay.example/v1/push".into(),
        token: "ab".repeat(32),
        environment: PushEnvironment::Sandbox,
        key: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=".into(),
        kinds: vec![PushKind::TurnFinished, PushKind::Approval],
    }
}

#[test]
fn a_device_registers_for_pushes_and_its_presence_follows_its_channels() {
    let host_dir = tempfile::tempdir().unwrap();
    let registry = SessionRegistry::new();
    let host = start_host(&host_dir, Arc::clone(&registry));

    runtime().block_on(async {
        let (paired, key) = paired_client(&host).await;
        let id = key.id();

        assert_eq!(host.presence(&id), Presence::Paired);

        let client = remote(paired, key);

        client.register_push(&registration()).await.unwrap();

        assert_eq!(host.devices()[0].push, Some(registration()));
        assert_eq!(host.presence(&id), Presence::Connected);

        // A plain http forwarder would leak nothing, but it is not what the
        // app registers, so it is a malformed registration.
        let mut plain = registration();

        plain.endpoint = "http://relay.example/v1/push".into();

        assert!(client.register_push(&plain).await.is_err());

        // A dropped link stays connected through the grace period. The
        // client stops first, or it would reconnect at once.
        client.shutdown();
        host.drop_connections();

        timeout(WAIT, async {
            while host.connected_devices().len() == 1 {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();

        assert_eq!(host.presence(&id), Presence::Connected);

        // The person using the host sends the device back to paired. The
        // channel's task records the close after the host dropped it from
        // its list, and a device still closing counts as connected, so the
        // signal repeats until the close has been recorded.
        timeout(WAIT, async {
            while host.presence(&id) != Presence::Paired {
                registry.note_local_use();

                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    });
}

#[test]
fn a_device_that_unregisters_gets_no_pushes() {
    let host_dir = tempfile::tempdir().unwrap();
    let host = start_host(&host_dir, SessionRegistry::new());

    runtime().block_on(async {
        let (paired, key) = paired_client(&host).await;
        let client = remote(paired, key);

        client.register_push(&registration()).await.unwrap();
        client.unregister_push().await.unwrap();

        assert_eq!(host.devices()[0].push, None);
    });
}

#[test]
fn a_device_learns_the_hosts_lan_addresses_and_a_stale_one_does_not_hold_it_up() {
    let host_dir = tempfile::tempdir().unwrap();
    let host = start_host(&host_dir, SessionRegistry::new());
    let port = host.local_addr().port();
    let address = format!("127.0.0.1:{port}");

    let advertised: Vec<String> = lan_addresses()
        .into_iter()
        .map(|ip| format!("{ip}:{port}"))
        .collect();

    runtime().block_on(async {
        let (mut paired, key) = paired_client(&host).await;

        // Pairing hands over the host's addresses behind the one used.
        let mut expected = vec![address.clone()];

        expected.extend(advertised.iter().cloned());
        expected.truncate(4);

        assert_eq!(paired.lan_hints, expected);

        // An address the host no longer has, first in line: 192.0.2.0/24 is
        // reserved for documentation and routes nowhere.
        paired.lan_hints = vec!["192.0.2.1:47470".into(), address.clone()];

        let (records, mut records_rx) = mpsc::unbounded_channel();

        let client = RemoteHost::new(paired, key, "0.0.0".into(), Retry::Forever, move |record| {
            let _ = records.send(record);
        });

        let started = Instant::now();

        client.list_sessions().await.unwrap();

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the stale address held the connection up for {:?}",
            started.elapsed()
        );

        // The host's list replaced the stale address.
        let record = timeout(WAIT, records_rx.recv()).await.unwrap().unwrap();

        assert_eq!(record.lan_hints[0], address);

        if !advertised.is_empty() {
            assert!(!record.lan_hints.contains(&"192.0.2.1:47470".to_owned()));
        }
    });
}

#[test]
fn parallel_attempts_that_reach_one_host_twice_still_connect() {
    let host_dir = tempfile::tempdir().unwrap();
    let host = start_host(&host_dir, SessionRegistry::new());
    let address = format!("127.0.0.1:{}", host.local_addr().port());

    runtime().block_on(async {
        let (mut paired, key) = paired_client(&host).await;

        // Several addresses of one host, as Wi-Fi and Ethernet give: the
        // host refuses every handshake that arrives after a newer one as a
        // replay, which must not read as the host refusing the device.
        paired.lan_hints = vec![address; 4];

        for _ in 0..5 {
            let client = remote(paired.clone(), Arc::clone(&key));

            client.list_sessions().await.unwrap();

            client.shutdown();
            host.drop_connections();
        }
    });
}

#[test]
fn a_limited_link_gives_up_on_an_unreachable_host_and_retries_on_request() {
    let host_dir = tempfile::tempdir().unwrap();
    let host = start_host(&host_dir, SessionRegistry::new());

    runtime().block_on(async {
        let (mut paired, key) = paired_client(&host).await;

        // A closed port refuses at once, and without the host nothing
        // answers a search on the LAN either.
        drop(host);

        paired.lan_hints = vec!["127.0.0.1:1".into()];

        let window = Duration::from_millis(500);

        let client = RemoteHost::new(
            paired,
            key,
            "0.0.0".into(),
            Retry::Limited {
                attempts: 2,
                window,
            },
            |_| {},
        );

        // A request fails once its round does, well before the request
        // timeout, instead of starting round after round until then.
        for _ in 0..2 {
            let started = Instant::now();

            assert!(client.list_sessions().await.is_err());
            assert_eq!(*client.status().borrow(), Status::Unreachable);

            let elapsed = started.elapsed();

            assert!(elapsed >= window, "the attempts are a window apart");
            assert!(elapsed < window * 6, "the round gave up after {elapsed:?}");
        }
    });
}
