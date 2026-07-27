//! Full-stack proof: a real HostService (ConPTY shell via the hub) serving a
//! client through the real relay Worker, including pairing, encrypted
//! terminal I/O, and detach/reattach-from-checkpoint after a disconnect.
//!
//! Requires `wrangler dev` on 127.0.0.1:8787 (run `npm run dev` in `relay/`):
//!
//! ```text
//! cargo test -p nmt_remote_net --test host_e2e -- --ignored
//! ```

#![cfg(windows)]

use std::time::Duration;
use std::{env, fs, process};

use nmt_remote_net::{
    AttachTarget, HostConfig, HostHandle, SessionByteEvent, client_connect_ik, client_connect_pair,
    list_remote_sessions, open_remote_session,
};
use nmt_remote_protocol::{
    ClientBound, Frame, HostBound, StaticKeypair, WireSessionOptions, generate_keypair,
};
use tokio::{task, time};

const RELAY: &str = "ws://127.0.0.1:8787/ws";
const TOKEN: &str = "test-token";
const MARKER: &str = "remote-e2e-marker";

#[tokio::test]
#[ignore = "requires `wrangler dev` running in relay/ (npm run dev)"]
async fn pair_open_shell_reconnect() {
    let data_dir = env::temp_dir().join(format!("nmt-host-e2e-{}", process::id()));
    let host = HostHandle::start(HostConfig {
        relay_url: RELAY.to_owned(),
        access_token: TOKEN.to_owned(),
        data_dir: data_dir.clone(),
    })
    .expect("host service starts");
    let host_public: Vec<u8> = host.public_key().to_vec();
    let host_id = host.host_id().to_owned();

    // Pair a fresh device. The host registers with the relay asynchronously,
    // so retry while the relay still reports it offline.
    let device = generate_keypair().unwrap();
    let code = host.begin_pairing();
    let mut channel = None;
    for _ in 0..40 {
        match client_connect_pair(&code, &device, "e2e-device").await {
            Ok(c) => {
                channel = Some(c);
                break;
            }
            Err(_) => time::sleep(Duration::from_millis(500)).await,
        }
    }
    let mut channel = channel.expect("pairing must succeed once the host is registered");
    assert!(
        host.list_devices().iter().any(|d| d.name == "e2e-device"),
        "paired device must be persisted"
    );

    // Open a shell and attach.
    channel
        .send_control(&HostBound::Open(WireSessionOptions {
            shell: Some("cmd.exe".into()),
            working_directory: None,
            cols: 100,
            rows: 30,
        }))
        .await
        .unwrap();
    let ClientBound::Opened { session_id } = channel.recv_control().await.unwrap() else {
        panic!("expected Opened");
    };
    channel
        .send_control(&HostBound::Attach { session_id })
        .await
        .unwrap();
    let ClientBound::Attached(_snapshot) = channel.recv_control().await.unwrap() else {
        panic!("expected Attached");
    };

    // Type a marker command and wait for it to come back as output.
    channel
        .send(&Frame::Input {
            session_id,
            data: format!("echo {MARKER}\r").into_bytes(),
        })
        .await
        .unwrap();
    let mut seen = Vec::new();
    time::timeout(Duration::from_secs(30), async {
        loop {
            if let Frame::Output { data, .. } = channel.recv().await.unwrap() {
                seen.extend_from_slice(&data);
                // Marker must appear beyond the local echo of the typed
                // command, i.e. twice: once echoed, once as command output.
                let text = String::from_utf8_lossy(&seen);
                if text.matches(MARKER).count() >= 2 {
                    return;
                }
            }
        }
    })
    .await
    .expect("marker output must arrive");

    // Drop the connection entirely; the shell must survive.
    drop(channel);
    time::sleep(Duration::from_secs(1)).await;

    // Reconnect as the now-authorized device (IK) and reattach: the fresh
    // checkpoint must contain the pre-disconnect screen, marker included.
    let mut channel = client_connect_ik(RELAY, &host_id, &host_public, &device)
        .await
        .expect("IK reconnect must succeed for a paired device");
    channel
        .send_control(&HostBound::Attach { session_id })
        .await
        .unwrap();
    let ClientBound::Attached(snapshot) = channel.recv_control().await.unwrap() else {
        panic!("expected Attached after reconnect");
    };
    let vt = String::from_utf8_lossy(&snapshot.vt);
    assert!(
        vt.contains(MARKER),
        "reattach snapshot must contain pre-disconnect output; got {} bytes",
        snapshot.vt.len()
    );

    channel
        .send_control(&HostBound::Kill { session_id })
        .await
        .unwrap();
    host.shutdown();
    fs::remove_dir_all(&data_dir).ok();
}

/// Proves the synchronous client runtime that `NetPty` sits on: open a session
/// via `open_remote_session`, drive it through the std byte-stream API, and
/// list sessions over a separate short-lived connection.
#[tokio::test]
#[ignore = "requires `wrangler dev` running in relay/ (npm run dev)"]
async fn client_runtime_byte_stream() {
    let data_dir = env::temp_dir().join(format!("nmt-host-e2e-client-{}", process::id()));
    let host = HostHandle::start(HostConfig {
        relay_url: RELAY.to_owned(),
        access_token: TOKEN.to_owned(),
        data_dir: data_dir.clone(),
    })
    .expect("host service starts");
    let host_public: Vec<u8> = host.public_key().to_vec();
    let host_id = host.host_id().to_owned();

    // Pair once so the device can use the IK client runtime.
    let device = generate_keypair().unwrap();
    let code = host.begin_pairing();
    for attempt in 0..40 {
        match client_connect_pair(&code, &device, "runtime-device").await {
            Ok(_) => break,
            Err(e) if attempt == 39 => panic!("pairing never succeeded: {e}"),
            Err(_) => time::sleep(Duration::from_millis(500)).await,
        }
    }

    // The synchronous runtime runs on its own threads; drive it off-blocking.
    let dev = StaticKeypair {
        private: device.private.clone(),
        public: device.public.clone(),
    };
    let (relay, hid, hpub) = (RELAY.to_owned(), host_id.clone(), host_public.clone());
    let session = task::spawn_blocking(move || {
        open_remote_session(
            relay,
            hid,
            hpub,
            dev,
            AttachTarget::Open(WireSessionOptions {
                shell: Some("cmd.exe".into()),
                working_directory: None,
                cols: 100,
                rows: 30,
            }),
        )
    })
    .await
    .unwrap()
    .expect("client runtime attaches");

    session.send_input(format!("echo {MARKER}\r").into_bytes());
    let seen = task::spawn_blocking(move || {
        let mut buf = Vec::new();
        loop {
            match session.output().recv_timeout(Duration::from_secs(30)) {
                Ok(SessionByteEvent::Output(data)) => {
                    buf.extend_from_slice(&data);
                    if String::from_utf8_lossy(&buf).matches(MARKER).count() >= 2 {
                        return buf;
                    }
                }
                Ok(SessionByteEvent::Exited) => return buf,
                Err(_) => return buf,
            }
        }
    })
    .await
    .unwrap();
    assert!(
        String::from_utf8_lossy(&seen).matches(MARKER).count() >= 2,
        "runtime byte stream must carry command output"
    );

    // Listing over a fresh connection sees the still-running session.
    let dev = StaticKeypair {
        private: device.private.clone(),
        public: device.public.clone(),
    };
    let (relay, hid, hpub) = (RELAY.to_owned(), host_id.clone(), host_public.clone());
    let sessions = task::spawn_blocking(move || list_remote_sessions(relay, hid, hpub, dev))
        .await
        .unwrap()
        .expect("list succeeds");
    assert!(!sessions.is_empty(), "listing must show the open session");

    host.shutdown();
    fs::remove_dir_all(&data_dir).ok();
}

/// Proves the client runtime survives losing its transport: the relay is
/// restarted mid-session, and the same `RemoteSession` handle keeps working
/// because the runtime re-attaches to the shell the host kept running.
///
/// Fault injection is manual — restart `wrangler dev` during the pause the
/// test prints — so it only runs when asked for by name and environment,
/// keeping its 45-second window out of the normal `--ignored` sweep:
///
/// ```text
/// NMT_RELAY_BOUNCE=1 cargo test -p nmt_remote_net --test host_e2e resumes_after -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "manual: restart `wrangler dev` while the test pauses"]
async fn client_runtime_resumes_after_transport_loss() {
    if env::var("NMT_RELAY_BOUNCE").is_err() {
        println!("skipped: set NMT_RELAY_BOUNCE=1 and bounce the relay during the pause");
        return;
    }
    let data_dir = env::temp_dir().join(format!("nmt-host-e2e-resume-{}", process::id()));
    let host = HostHandle::start(HostConfig {
        relay_url: RELAY.to_owned(),
        access_token: TOKEN.to_owned(),
        data_dir: data_dir.clone(),
    })
    .expect("host service starts");
    let host_public: Vec<u8> = host.public_key().to_vec();
    let host_id = host.host_id().to_owned();

    let device = generate_keypair().unwrap();
    let code = host.begin_pairing();
    for attempt in 0..40 {
        match client_connect_pair(&code, &device, "resume-device").await {
            Ok(_) => break,
            Err(e) if attempt == 39 => panic!("pairing never succeeded: {e}"),
            Err(_) => time::sleep(Duration::from_millis(500)).await,
        }
    }

    let dev = StaticKeypair {
        private: device.private.clone(),
        public: device.public.clone(),
    };
    let (relay, hid, hpub) = (RELAY.to_owned(), host_id.clone(), host_public.clone());
    let session = task::spawn_blocking(move || {
        open_remote_session(
            relay,
            hid,
            hpub,
            dev,
            AttachTarget::Open(WireSessionOptions {
                shell: Some("cmd.exe".into()),
                working_directory: None,
                cols: 100,
                rows: 30,
            }),
        )
    })
    .await
    .unwrap()
    .expect("client runtime attaches");

    println!("--- restart `wrangler dev` now; resuming in 45s ---");
    time::sleep(Duration::from_secs(45)).await;

    // Input after the restart can only arrive if the runtime re-attached.
    session.send_input(format!("echo {MARKER}\r").into_bytes());
    let seen = task::spawn_blocking(move || {
        let mut buf = Vec::new();
        loop {
            match session.output().recv_timeout(Duration::from_secs(60)) {
                Ok(SessionByteEvent::Output(data)) => {
                    buf.extend_from_slice(&data);
                    if String::from_utf8_lossy(&buf).matches(MARKER).count() >= 2 {
                        return buf;
                    }
                }
                Ok(SessionByteEvent::Exited) => panic!("session died instead of resuming"),
                Err(_) => return buf,
            }
        }
    })
    .await
    .unwrap();
    assert!(
        String::from_utf8_lossy(&seen).matches(MARKER).count() >= 2,
        "the resumed session must carry command output"
    );

    host.shutdown();
    fs::remove_dir_all(&data_dir).ok();
}

#[tokio::test]
#[ignore = "requires `wrangler dev` running in relay/ (npm run dev)"]
async fn unpaired_device_rejected() {
    let data_dir = env::temp_dir().join(format!("nmt-host-e2e-rej-{}", process::id()));
    let host = HostHandle::start(HostConfig {
        relay_url: RELAY.to_owned(),
        access_token: TOKEN.to_owned(),
        data_dir: data_dir.clone(),
    })
    .expect("host service starts");
    let host_public: Vec<u8> = host.public_key().to_vec();
    let host_id = host.host_id().to_owned();
    time::sleep(Duration::from_secs(2)).await;

    // Never paired: the IK handshake must die without a reply.
    let intruder = generate_keypair().unwrap();
    let result = client_connect_ik(RELAY, &host_id, &host_public, &intruder).await;
    assert!(
        result.is_err(),
        "unauthorized device must not get a channel"
    );

    // A wrong pairing token must be refused even over a valid XX handshake.
    let code = nmt_remote_protocol::PairingCode {
        relay_url: RELAY.to_owned(),
        host_id: host_id.clone(),
        host_public_key: host_public.as_slice().try_into().unwrap(),
        token: [0u8; 16],
    };
    let result = client_connect_pair(&code, &intruder, "intruder").await;
    assert!(result.is_err(), "bogus token must not pair");
    assert!(host.list_devices().is_empty(), "no device may be persisted");

    host.shutdown();
    fs::remove_dir_all(&data_dir).ok();
}
