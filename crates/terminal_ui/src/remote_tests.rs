#![cfg(windows)]
use std::{env, fs, process, thread, time};

use nmt_config::colors::Colors;
use nmt_remote_net::net_pty::terminal_session;
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

use crate::frame_source::TerminalFrameSource;
use crate::layout::live_frame_text;
use crate::pane_model::FrameTheme;
use crate::wake::wake_channel;

/// End-to-end proof that a remote session renders through `NetPty`: start a
/// host, pair, attach, type a command, and confirm its output reaches the
/// engine's screen state. Requires `wrangler dev` on 127.0.0.1:8787
/// (`npm run dev` in the repo root), so it is ignored by default:
///
/// ```text
/// cargo test -p nmt_terminal_ui remote_session_renders_through_net_pty -- --ignored
/// ```
#[cfg(windows)]
#[test]
#[ignore = "requires `wrangler dev` running (npm run dev in the repo root)"]
fn remote_session_renders_through_net_pty() {
    use nmt_remote_net::{
        AttachTarget, HostConfig, HostHandle, ProtocolSessionOptions, StaticKeypair,
        client_connect_pair, generate_keypair, open_remote_session,
    };

    const RELAY: &str = "ws://127.0.0.1:8787/ws";
    const TOKEN: &str = "test-token";
    const MARKER: &str = "netpty-render-marker";

    let data_dir = env::temp_dir().join(format!("nmt-netpty-{}", process::id()));

    let host = HostHandle::start(HostConfig {
        relay_url: RELAY.to_owned(),
        access_token: TOKEN.to_owned(),
        data_dir: data_dir.clone(),
    })
    .expect("host starts");

    let host_public = host.public_key().to_vec();
    let host_id = host.host_id().to_owned();

    // Pair a device (retry while the host finishes registering with relay).
    let device = generate_keypair().unwrap();
    let code = host.begin_pairing();
    let rt = tokio_runtime();
    let mut paired = false;

    for _ in 0..40 {
        let dev = StaticKeypair {
            private: device.private.clone(),
            public: device.public.clone(),
        };

        if rt
            .block_on(client_connect_pair(&code, &dev, "netpty-test"))
            .is_ok()
        {
            paired = true;
            break;
        }

        thread::sleep(time::Duration::from_millis(500));
    }

    assert!(paired, "pairing must succeed");

    let remote = open_remote_session(
        RELAY.to_owned(),
        host_id,
        host_public,
        device,
        AttachTarget::Open(ProtocolSessionOptions {
            shell: Some("cmd.exe".into()),
            working_directory: None,
            cols: 100,
            rows: 30,
        }),
    )
    .expect("attach");

    let mut surface = TerminalFrameSource::attach(wake_channel().0, 1, |observer| {
        terminal_session(remote, 1, Colors::default(), Some(observer))
    })
    .expect("remote session");

    surface
        .session
        .write_input(format!("echo {MARKER}\r").as_bytes());

    let deadline = time::Instant::now() + time::Duration::from_secs(30);
    let mut rendered = false;

    while time::Instant::now() < deadline {
        let vt = live_frame_text(&surface.frame(None, &FrameTheme::default())).unwrap_or_default();

        if vt.contains(MARKER) {
            rendered = true;
            break;
        }

        thread::sleep(time::Duration::from_millis(200));
    }

    assert!(
        rendered,
        "command output must render through NetPty into the engine"
    );

    host.shutdown();
    fs::remove_dir_all(&data_dir).ok();
}

#[cfg(test)]
#[cfg(windows)]
fn tokio_runtime() -> Runtime {
    RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}
