#![cfg(windows)]

use std::time::{Duration, Instant};

use nmt_remote_session_hub::{RemoteSessionHub, SessionOptions};

#[test]
fn session_survives_detach_and_reconnects_from_a_vt_checkpoint() {
    let hub = RemoteSessionHub::new();
    let id = hub
        .open(SessionOptions {
            shell: "powershell.exe".to_owned(),
            args: vec!["-NoLogo".to_owned(), "-NoProfile".to_owned()],
            ..SessionOptions::default()
        })
        .expect("start ConPTY session");

    let first = hub.attach(id).expect("attach first client");
    hub.write_input(id, b"Write-Output NMT_REMOTE_FIRST\r")
        .expect("write first command");
    wait_for_live_output(&first, b"NMT_REMOTE_FIRST");
    drop(first);

    hub.write_input(id, b"Write-Output NMT_REMOTE_DETACHED\r")
        .expect("write while detached");
    hub.resize(id, 100, 30).expect("resize detached session");

    let deadline = Instant::now() + Duration::from_secs(10);
    let second = loop {
        let subscription = hub.attach(id).expect("reattach client");
        if subscription
            .snapshot()
            .vt
            .windows(b"NMT_REMOTE_DETACHED".len())
            .any(|window| window == b"NMT_REMOTE_DETACHED")
        {
            break subscription;
        }
        assert!(
            Instant::now() < deadline,
            "detached output never reached checkpoint"
        );
        drop(subscription);
        std::thread::sleep(Duration::from_millis(25));
    };

    assert_eq!((second.snapshot().cols, second.snapshot().rows), (100, 30));
    assert_eq!(hub.list_sessions()[0].attached_clients, 1);
    hub.kill(id).expect("kill session");
    wait_for_exit(&second);
    drop(second);
    assert!(hub.list_sessions().is_empty());
}

fn wait_for_live_output(subscription: &nmt_remote_session_hub::SessionSubscription, needle: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut output = Vec::new();
    while Instant::now() < deadline {
        if let Ok(nmt_remote_session_hub::SessionEvent::Output { data, .. }) = subscription
            .events()
            .recv_timeout(Duration::from_millis(250))
        {
            output.extend_from_slice(&data);
            if output.windows(needle.len()).any(|window| window == needle) {
                return;
            }
        }
    }
    panic!(
        "live output did not contain {}",
        String::from_utf8_lossy(needle)
    );
}

fn wait_for_exit(subscription: &nmt_remote_session_hub::SessionSubscription) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if matches!(
            subscription
                .events()
                .recv_timeout(Duration::from_millis(100)),
            Ok(nmt_remote_session_hub::SessionEvent::Exited { .. })
        ) {
            return;
        }
    }
    panic!("subscriber did not receive session exit");
}
