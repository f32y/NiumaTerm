use std::sync::Arc;
use std::{collections, env, fs, process, sync, thread, time};

use base64::engine::general_purpose::STANDARD;
use nmt_terminal::block_store::BlockStore;
use nmt_terminal::event::{BlockEvent, TerminalEvent};
use parking_lot::Mutex;
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

use crate::error::EngineErrorCode;
use crate::terminal;
use crate::terminal::session::{
    HostEvent, HostEventQueue, SessionGraphics, TerminalEventProxy, TerminalSession,
    TerminalSessionConfig,
};
use crate::terminal::wake::Wake;
use crate::utils::POWERSHELL_INTEGRATION;

/// End-to-end proof that a remote session renders through `NetPty`: start a
/// host, pair, attach, type a command, and confirm its output reaches the
/// engine's screen state. Requires `wrangler dev` on 127.0.0.1:8787
/// (`npm run dev` in the repo root), so it is ignored by default:
///
/// ```text
/// cargo test -p app remote_session_renders_through_net_pty -- --ignored
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

    let session = TerminalSession::new_remote(remote, 1, None).expect("remote session");
    session.write_input(format!("echo {MARKER}\r").as_bytes());

    let deadline = time::Instant::now() + time::Duration::from_secs(30);
    let mut rendered = false;
    while time::Instant::now() < deadline {
        let vt = session.engine.lock().format_vt_state().unwrap_or_default();
        if String::from_utf8_lossy(&vt).contains(MARKER) {
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
fn tokio_runtime() -> Runtime {
    RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

#[test]
fn trusted_prompt_integration_requires_injected_powershell_startup() {
    assert!(TerminalSessionConfig::default().has_trusted_prompt_integration());

    assert!(
        TerminalSessionConfig {
            shell: Some("pwsh.exe".into()),
            ..TerminalSessionConfig::default()
        }
        .has_trusted_prompt_integration()
    );

    assert!(
        !TerminalSessionConfig {
            shell: Some("pwsh.exe".into()),
            args: vec!["-NoLogo".into()],
            ..TerminalSessionConfig::default()
        }
        .has_trusted_prompt_integration()
    );

    assert!(
        !TerminalSessionConfig {
            shell: Some("cmd.exe".into()),
            ..TerminalSessionConfig::default()
        }
        .has_trusted_prompt_integration()
    );
}

#[test]
fn powershell_bootstrap_is_passed_as_utf16_encoded_command() {
    use base64::Engine as _;

    let config = TerminalSessionConfig::default().with_shell_integration();
    let encoded = &config.args[2];
    let bytes = STANDARD.decode(encoded).unwrap();
    let utf16: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();

    assert_eq!(config.args[0], "-NoExit");
    assert_eq!(config.args[1], "-EncodedCommand");
    assert_eq!(
        String::from_utf16(utf16.as_slice()).unwrap(),
        POWERSHELL_INTEGRATION
    );
}

/// Creating a session with a non-existent shell returns a structured
/// `PtySpawn` error rather than a bare null so callers retain the failure cause.
#[test]
fn bad_shell_returns_structured_error() {
    let config = TerminalSessionConfig {
        shell: Some("this-shell-does-not-exist-xyz.exe".into()),
        ..TerminalSessionConfig::default()
    };
    let err = TerminalSession::new_internal(&config, 1, None)
        .err()
        .expect("a non-existent shell must fail");
    assert_eq!(err.code, EngineErrorCode::PtySpawn);
    assert!(!err.message.is_empty());
}

#[test]
fn restorable_tab_state_keeps_original_launch_command() {
    let config = TerminalSessionConfig {
        shell: Some("pwsh.exe".to_string()),
        working_dir: Some("C:/Projects/example".to_string()),
        ..TerminalSessionConfig::default()
    };

    let state = config.restorable_tab_state();
    let integrated = config.with_shell_integration();

    assert_eq!(state.shell.as_deref(), Some("pwsh.exe"));
    assert!(state.args.is_empty());
    assert_eq!(state.cwd.as_deref(), Some("C:/Projects/example"));
    assert!(!integrated.args.is_empty());
}

/// `NiumaTermEventListener` maps user-visible `TerminalEvent`s onto the host-event queue
/// the shell drains, including title (incl. reset → empty), bell, exit, and
/// desktop notification.
#[test]
fn host_events_map_from_terminal_events() {
    use nmt_terminal::event::{EventListener, TerminalEvent, WindowId};

    let events: HostEventQueue = Arc::new(Mutex::new(collections::VecDeque::new()));
    let listener = TerminalEventProxy::new(
        Arc::clone(&events),
        Arc::new(Mutex::new(BlockStore::default())),
        Arc::new(Mutex::new(terminal::graphics::GenerationStore::new())),
        Arc::new(sync::atomic::AtomicUsize::new(0)),
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(Mutex::new(None)),
        Arc::new(Mutex::new(false)),
        Default::default(),
        1,
        None,
    );
    let wid = WindowId::dummy();

    listener.send_event(TerminalEvent::Title("t".into()), wid);
    listener.send_event(TerminalEvent::ResetTitle, wid);
    listener.send_event(TerminalEvent::Bell, wid);
    listener.send_event(TerminalEvent::CloseTerminal(0), wid);
    listener.send_event(
        TerminalEvent::DesktopNotification {
            title: "T".into(),
            body: "B".into(),
        },
        wid,
    );
    listener.send_event(TerminalEvent::PromptBoundaryTrusted(true), wid);

    let q = events.lock();
    let v: Vec<&HostEvent> = q.iter().collect();
    assert!(matches!(v[0], HostEvent::Title(s) if s == "t"));
    assert!(matches!(v[1], HostEvent::Title(s) if s.is_empty()));
    assert!(matches!(v[2], HostEvent::Bell));
    assert!(matches!(v[3], HostEvent::Exit));
    assert!(matches!(v[4], HostEvent::Notification { title, body } if title == "T" && body == "B"));
    assert!(matches!(v[5], HostEvent::PromptBoundaryTrusted(true)));
}

#[test]
fn osc_notification_drains_into_shared_exact_notification_lifecycle() {
    use std::time::Instant;

    use nmt_agent_utils::{
        AgentActivityPolicy, AgentMonitor, AgentRoute, AgentRuntimeStatus, request_native_delivery,
    };
    use nmt_terminal::event::{EventListener, TerminalEvent, WindowId};

    let events: HostEventQueue = Arc::new(Mutex::new(collections::VecDeque::new()));
    let listener = TerminalEventProxy::new(
        Arc::clone(&events),
        Arc::new(Mutex::new(BlockStore::default())),
        Arc::new(Mutex::new(terminal::graphics::GenerationStore::new())),
        Arc::new(sync::atomic::AtomicUsize::new(0)),
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(Mutex::new(None)),
        Arc::new(Mutex::new(false)),
        Default::default(),
        1,
        None,
    );
    listener.send_event(
        TerminalEvent::DesktopNotification {
            title: "T".repeat(300),
            body: "B".repeat(5_000),
        },
        WindowId::dummy(),
    );

    let route = AgentRoute::parse("osc-route").unwrap();
    let mut monitor = AgentMonitor::new("process");
    monitor.register_route(
        route.clone(),
        AgentActivityPolicy::ExpireAfterInactivity,
        Instant::now(),
    );
    let event = events.lock().pop_front().unwrap();
    let HostEvent::Notification { title, body } = event else {
        panic!("expected OSC notification host event");
    };
    monitor.notify(&route, &title, &body);

    let notification = monitor.notification(&route).unwrap().clone();
    assert_eq!(notification.title.chars().count(), 256);
    assert_eq!(notification.body.chars().count(), 4_096);
    assert_eq!(monitor.project([&route]).status, AgentRuntimeStatus::Idle);
    assert!(request_native_delivery(None, &route));
    assert!(monitor.mark_native_requested(&route, &notification.id));
    assert!(
        monitor
            .acknowledge(&route, &notification.id)
            .visible_changed
    );
    assert_eq!(monitor.project([&route]).unread_count, 0);
}

/// In-flight lifecycle: CommandStarted sets the running block, CommandFinished
/// finalizes it in place; trust loss and exit clear it without appending a block.
#[test]
fn in_flight_block_lifecycle() {
    use std::time::SystemTime;

    use nmt_terminal::event::{
        CommandCapture, CommandStart, EventListener, TerminalEvent, WindowId,
    };

    fn start(cmd: &str) -> CommandStart {
        CommandStart {
            seq: 0,
            command: cmd.to_string(),
            cwd: Some("C:/w".into()),
            started_at: SystemTime::now(),
        }
    }
    fn capture(cmd: &str) -> CommandCapture {
        let now = SystemTime::now();
        CommandCapture {
            seq: 0,
            command: cmd.to_string(),
            exit_code: Some(0),
            cwd: Some("C:/w".into()),
            started_at: now,
            ended_at: now,
        }
    }

    let events: HostEventQueue = Arc::new(Mutex::new(collections::VecDeque::new()));
    let in_flight = Arc::new(Mutex::new(None));
    let open_prompt = Arc::new(Mutex::new(false));
    let proxy = TerminalEventProxy::new(
        Arc::clone(&events),
        Arc::new(Mutex::new(BlockStore::default())),
        Arc::new(Mutex::new(terminal::graphics::GenerationStore::new())),
        Arc::new(sync::atomic::AtomicUsize::new(0)),
        Arc::new(Mutex::new(Vec::new())),
        Arc::clone(&in_flight),
        Arc::clone(&open_prompt),
        Default::default(),
        1,
        None,
    );
    let wid = WindowId::dummy();

    proxy.send_event(TerminalEvent::PromptStarted, wid);
    assert!(*open_prompt.lock());

    // start -> finish: in-flight visible while running, then cleared.
    proxy.send_event(TerminalEvent::CommandStarted(start("sleep 5")), wid);
    assert!(!*open_prompt.lock(), "command start closes prompt");
    {
        let running = in_flight.lock().clone().expect("in-flight set");
        assert_eq!(running.command.as_str(), "sleep 5");
    }
    proxy.send_event(TerminalEvent::CommandFinished(capture("sleep 5")), wid);
    assert!(
        in_flight.lock().is_none(),
        "finished command clears live state"
    );

    // start -> trust loss: cleared.
    proxy.send_event(TerminalEvent::CommandStarted(start("nested")), wid);
    assert_eq!(in_flight.lock().clone().unwrap().command, "nested");
    proxy.send_event(TerminalEvent::PromptStarted, wid);
    assert!(*open_prompt.lock());
    proxy.send_event(TerminalEvent::PromptBoundaryTrusted(false), wid);
    assert!(
        in_flight.lock().is_none(),
        "trust loss drops the running block"
    );
    assert!(!*open_prompt.lock(), "trust loss closes prompt");

    // start -> exit: cleared as well.
    proxy.send_event(TerminalEvent::CommandStarted(start("hang")), wid);
    proxy.send_event(TerminalEvent::PromptStarted, wid);
    assert!(*open_prompt.lock());
    proxy.send_event(TerminalEvent::CloseTerminal(0), wid);
    assert!(in_flight.lock().is_none(), "exit drops the running block");
    assert!(!*open_prompt.lock(), "exit closes prompt");

    // The host queue saw the prompt and command events too, in order.
    let q = events.lock();
    assert!(matches!(&q[0], HostEvent::PromptStarted));
    assert!(matches!(&q[1], HostEvent::CommandStarted));
    assert!(matches!(&q[2], HostEvent::CommandFinished { .. }));
}

/// Block-split wiring: `BlockBatch` events feed the shared store, and
/// `CommandStarted`/`CommandFinished` metadata marries its segment by
/// `seq` even though the marks fire long before the segment's lines
/// scroll out of the active area.
#[test]
fn block_batches_and_seq_metadata_reach_the_block_store() {
    use std::time::SystemTime;

    use nmt_terminal::event::{
        BlockEvent, CommandCapture, CommandStart, EventListener, TerminalEvent, WindowId,
    };
    use nmt_terminal::ghostty::BlockHandle;

    let store = Arc::new(Mutex::new(BlockStore::default()));
    let proxy = TerminalEventProxy::new(
        Arc::new(Mutex::new(collections::VecDeque::new())),
        Arc::clone(&store),
        Arc::new(Mutex::new(terminal::graphics::GenerationStore::new())),
        Arc::new(sync::atomic::AtomicUsize::new(0)),
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(Mutex::new(None)),
        Arc::new(Mutex::new(false)),
        Default::default(),
        1,
        None,
    );
    let wid = WindowId::dummy();
    let now = SystemTime::now();

    // Marks fire first (write time)...
    proxy.send_event(
        TerminalEvent::CommandStarted(CommandStart {
            seq: 1,
            command: "cargo build".into(),
            cwd: Some("C:/w".into()),
            started_at: now,
        }),
        wid,
    );
    proxy.send_event(
        TerminalEvent::CommandFinished(CommandCapture {
            seq: 1,
            command: "cargo build".into(),
            exit_code: Some(0),
            cwd: Some("C:/w".into()),
            started_at: now,
            ended_at: now,
        }),
        wid,
    );
    // ...the item materializes later, at the block's finish. The batch is
    // staged and only flushed to the store on the read's damage wake, so
    // nothing lands until the following `TerminalDamaged`.
    proxy.send_event(
        TerminalEvent::BlockBatch(vec![BlockEvent::EngineBlock {
            seq: 1,
            handle: BlockHandle {
                id: 1,
                generation: 1,
            },
            rows: 3,
        }]),
        wid,
    );
    assert!(
        store.lock().items().is_empty(),
        "staged batch must not reach the store before the damage flush"
    );
    proxy.send_event(TerminalEvent::TerminalDamaged(1), wid);

    let store = store.lock();
    let items = store.items();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].seq, Some(1));
    assert_eq!(items[0].engine_rows(), 3);
    assert_eq!(items[0].meta.command.as_deref(), Some("cargo build"));
    assert_eq!(items[0].meta.exit_code, Some(0));
}

/// Build a `TerminalEventProxy` whose state Arcs the test retains, plus a wake
/// collector. `id` is the route so `UpdateGraphics` routing can be exercised.
fn graphics_proxy(id: u64) -> (TerminalEventProxy, GraphicsProbes) {
    use crate::terminal::graphics::GenerationStore;
    use crate::terminal::wake::WakeSender;

    let events: HostEventQueue = Arc::new(Mutex::new(collections::VecDeque::new()));
    let block_store = Arc::new(Mutex::new(BlockStore::default()));
    let generation_store = Arc::new(Mutex::new(GenerationStore::new()));
    let staged_blocks = Arc::new(Mutex::new(Vec::new()));
    let wakes = Arc::new(Mutex::new(Vec::new()));
    let wakes_for_sender = Arc::clone(&wakes);
    let proxy = TerminalEventProxy::new(
        Arc::clone(&events),
        Arc::clone(&block_store),
        Arc::clone(&generation_store),
        Arc::new(sync::atomic::AtomicUsize::new(0)),
        Arc::clone(&staged_blocks),
        Arc::new(Mutex::new(None)),
        Arc::new(Mutex::new(false)),
        Default::default(),
        id,
        Some(WakeSender::from_fn(move |w| {
            wakes_for_sender.lock().push(w)
        })),
    );
    (
        proxy,
        GraphicsProbes {
            events,
            block_store,
            generation_store,
            staged_blocks,
            wakes,
        },
    )
}

struct GraphicsProbes {
    events: HostEventQueue,
    block_store: Arc<Mutex<BlockStore>>,
    generation_store: SessionGraphics,
    staged_blocks: Arc<Mutex<Vec<BlockEvent>>>,
    wakes: Arc<Mutex<Vec<Wake>>>,
}

fn rgba_update(route_id: usize, image_id: u32, w: usize, h: usize) -> TerminalEvent {
    use nmt_terminal::graphics::{ColorType, GraphicData, GraphicId, UpdateQueues};
    let data = GraphicData {
        id: GraphicId(image_id as u64),
        width: w,
        height: h,
        color_type: ColorType::Rgba,
        pixels: vec![0u8; w * h * 4],
        is_opaque: true,
        resize: None,
        display_width: None,
        display_height: None,
        transmit_time: time::Instant::now(),
    };
    TerminalEvent::UpdateGraphics {
        route_id,
        queues: UpdateQueues {
            pending: Vec::new(),
            pending_images: vec![(image_id, data)],
            remove_queue: Vec::new(),
        },
    }
}

/// `UpdateGraphics` installs a live generation and wakes for content,
/// but never enqueues a host event; a mismatched route is ignored entirely.
#[test]
fn graphics_events_bypass_host_queue_and_are_route_scoped() {
    use nmt_terminal::event::{EventListener, WindowId};
    let (proxy, p) = graphics_proxy(4);
    let wid = WindowId::dummy();

    proxy.send_event(rgba_update(4, 7, 2, 2), wid);
    assert!(
        p.events.lock().is_empty(),
        "graphics never enters the host queue"
    );
    assert!(
        p.generation_store.lock().get(7).is_some(),
        "generation installed"
    );
    assert_eq!(*p.wakes.lock(), vec![Wake::Content(4)], "one content wake");

    // A cross-session route is dropped: no install, no wake.
    proxy.send_event(rgba_update(999, 8, 2, 2), wid);
    assert!(
        p.generation_store.lock().get(8).is_none(),
        "wrong route ignored"
    );
    assert_eq!(p.wakes.lock().len(), 1, "no wake for wrong route");
}

/// Sustained output cannot grow an unbounded UI-facing queue. Each
/// read's staged block events flush on their damage wake, and the host-event queue is
/// never touched, so neither the staging buffer nor the host queue accumulates.
#[test]
fn sustained_output_does_not_grow_ui_queue() {
    use nmt_terminal::event::{BlockEvent, EventListener, WindowId};
    use nmt_terminal::ghostty::BlockHandle;
    let (proxy, p) = graphics_proxy(1);
    let wid = WindowId::dummy();

    for seq in 0..1000u64 {
        proxy.send_event(
            TerminalEvent::BlockBatch(vec![BlockEvent::EngineBlocksSync(vec![(
                BlockHandle {
                    id: seq,
                    generation: 1,
                },
                1,
            )])]),
            wid,
        );
        proxy.send_event(rgba_update(1, 1, 1, 1), wid);
        proxy.send_event(TerminalEvent::TerminalDamaged(1), wid);
        // After each read's damage flush the staging buffer is empty again.
        assert!(
            p.staged_blocks.lock().is_empty(),
            "staging bounded to one read"
        );
    }
    assert!(
        p.events.lock().is_empty(),
        "host queue never grew from graphics/block events"
    );
    // The live generation is a single replaced entry, not 1000 accumulated ones.
    assert_eq!(
        p.generation_store.lock().len(),
        1,
        "one live generation, replaced"
    );
}

/// On the UI wake, active (live generation) and frozen (block-store
/// history) image state are both present — the read installed the generation
/// before flushing the block batch that froze the same content.
#[test]
fn active_and_frozen_state_coherent_at_wake() {
    use nmt_terminal::event::{BlockEvent, EventListener, WindowId};
    use nmt_terminal::ghostty::BlockHandle;
    let (proxy, p) = graphics_proxy(1);
    let wid = WindowId::dummy();

    // Order within a read: block event staged, then generation installed,
    // then damage.
    proxy.send_event(
        TerminalEvent::BlockBatch(vec![BlockEvent::EngineBlock {
            seq: 1,
            handle: BlockHandle {
                id: 1,
                generation: 1,
            },
            rows: 2,
        }]),
        wid,
    );
    proxy.send_event(rgba_update(1, 42, 2, 2), wid);
    // Before the flush the frozen row is not yet in the store.
    assert!(p.block_store.lock().items().is_empty());
    proxy.send_event(TerminalEvent::TerminalDamaged(1), wid);

    // At the wake both sides are coherent: live generation present AND frozen row
    // committed to history.
    assert!(
        p.generation_store.lock().get(42).is_some(),
        "live generation present"
    );
    assert_eq!(
        p.block_store.lock().items().len(),
        1,
        "frozen history committed by the same wake"
    );
    assert!(p.wakes.lock().contains(&Wake::Content(1)));
}
