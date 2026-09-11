use std::collections::HashMap;
use std::sync::Arc;
#[cfg(windows)]
use std::thread;
use std::time;

#[cfg(windows)]
use base64::engine::general_purpose::STANDARD;
use nmt_config::active_colors;
#[cfg(windows)]
use nmt_platform::windows::powershell::INTEGRATION_SCRIPT;
use parking_lot::Mutex;

use crate::event::TerminalEvent;
use crate::graphics::UpdateQueues;
use crate::session::error::EngineErrorCode;
use crate::session::{
    HostEvent, SessionChange, SessionObserver, SessionSharedState, TerminalEventProxy,
    TerminalSession, TerminalSessionConfig,
};

/// Which shells carry a trusted OSC 133 integration is a platform answer, so
/// the acceptance cases live under the platform that provides the script.
#[cfg(windows)]
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

#[cfg(windows)]
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
        INTEGRATION_SCRIPT
    );
}

/// zsh is handed its integration by being typed at before the shell reads
/// anything, so the launch carries the bootstrap rather than a startup file
/// the shell would have to find. The configured launch command survives.
#[cfg(unix)]
#[test]
fn zsh_is_integrated_through_an_injected_bootstrap() {
    let config = TerminalSessionConfig {
        shell: Some("/bin/zsh".into()),
        ..TerminalSessionConfig::default()
    };

    assert!(config.has_trusted_prompt_integration());

    let integrated = config.with_shell_integration();

    assert!(integrated.bootstrap.is_some());
    assert!(integrated.environment_overrides.is_empty());

    // The launch has to suppress zsh's own startup files, since the bootstrap
    // replays them itself.
    assert!(integrated.args.contains(&String::from("-f")));
}

/// bash is handed its integration the same way zsh is, so the same two things
/// hold: the launch carries a bootstrap, and the configured launch command
/// survives into the restorable tab state.
#[cfg(unix)]
#[test]
fn bash_is_integrated_through_an_injected_bootstrap() {
    let config = TerminalSessionConfig {
        shell: Some("/bin/bash".into()),
        ..TerminalSessionConfig::default()
    };

    assert!(config.has_trusted_prompt_integration());

    let args = config.args.clone();
    let integrated = config.with_shell_integration();

    assert!(args.is_empty());
    assert!(integrated.bootstrap.is_some());

    // The launch has to suppress bash's own startup files, since the bootstrap
    // replays them itself.
    assert!(integrated.args.contains(&String::from("--norc")));
}

/// A shell the platform has no integration for must be told so. Claiming a
/// trusted prompt anyway would have the terminal trust boundaries nothing
/// emits.
#[cfg(unix)]
#[test]
fn a_shell_without_an_integration_claims_no_trusted_prompt() {
    for shell in ["/bin/sh", "/usr/local/bin/fish", "/usr/bin/tcsh"] {
        let config = TerminalSessionConfig {
            shell: Some(shell.into()),
            ..TerminalSessionConfig::default()
        };

        assert!(!config.has_trusted_prompt_integration(), "{shell}");

        let integrated = config.with_shell_integration();

        assert!(integrated.args.is_empty(), "{shell}");
        assert!(integrated.environment_overrides.is_empty(), "{shell}");
        assert!(integrated.bootstrap.is_none(), "{shell}");
    }
}

/// Caller-supplied args are the user's own launch command; injecting an
/// integration would replace or contradict it.
#[cfg(unix)]
#[test]
fn explicit_args_suppress_the_zsh_integration() {
    let config = TerminalSessionConfig {
        shell: Some("/bin/zsh".into()),
        args: vec!["--no-rcs".into()],
        ..TerminalSessionConfig::default()
    };

    assert!(!config.has_trusted_prompt_integration());

    let integrated = config.with_shell_integration();

    assert_eq!(integrated.args, ["--no-rcs"]);
    assert!(integrated.environment_overrides.is_empty());
    assert!(integrated.bootstrap.is_none());
}

/// Creating a session with a non-existent shell returns a structured
/// `PtySpawn` error rather than a bare null so callers retain the failure cause.
#[test]
fn bad_shell_returns_structured_error() {
    let config = TerminalSessionConfig {
        shell: Some("this-shell-does-not-exist-xyz".into()),
        ..TerminalSessionConfig::default()
    };

    let err = TerminalSession::new(&config, 1, active_colors(), None)
        .err()
        .expect("a non-existent shell must fail");

    assert_eq!(err.code, EngineErrorCode::PtySpawn);
    assert!(!err.message.is_empty());
}

#[cfg(windows)]
#[test]
fn local_session_publishes_engine_output_and_host_events() {
    const ROUTE: u64 = 91_301;
    const MARKER: &str = "nmt-session-shared-state";

    let wakes = Arc::new(Mutex::new(Vec::new()));

    let session = TerminalSession::new(
        &TerminalSessionConfig {
            shell: Some("cmd.exe".into()),
            args: vec!["/D".into(), "/Q".into()],
            cols: 90,
            rows: 25,
            manage_process_tree: true,
            ..TerminalSessionConfig::default()
        },
        ROUTE,
        active_colors(),
        Some(Arc::new(TestObserver {
            wakes: wakes.clone(),
            ..Default::default()
        })),
    )
    .unwrap();

    assert!(session.engine_blocks());
    assert_eq!(session.snapshot().cols(), 90);
    assert_eq!(session.snapshot().rows(), 25);

    session.write_input(format!("title {MARKER}\r\necho {MARKER}\r\n").as_bytes());

    let deadline = time::Instant::now() + time::Duration::from_secs(10);
    let mut title_seen = false;

    loop {
        title_seen |= session
            .poll_events()
            .iter()
            .any(|event| matches!(event, HostEvent::Title(title) if title == MARKER));

        let snapshot = session.snapshot();
        let top = snapshot
            .viewport_top
            .expect("the viewport must have a top row");
        let output = (0..snapshot.rows())
            .map(|row| {
                session
                    .screen_row_text_in(&snapshot, top + row as u32)
                    .expect("visible rows must be available in the snapshot")
                    .text
            })
            .collect::<Vec<_>>()
            .join("\n");

        if title_seen && output.contains(MARKER) {
            break;
        }

        assert!(
            time::Instant::now() < deadline,
            "shell output and title must reach the session"
        );

        thread::sleep(time::Duration::from_millis(10));
    }

    let wakes = wakes.lock();

    assert!(wakes.contains(&SessionChange::Content));
    assert!(wakes.contains(&SessionChange::HostEvents));
}

/// `NiumaTermEventListener` maps user-visible `TerminalEvent`s onto the host-event queue
/// the shell drains, including title (incl. reset → empty), bell, exit, and
/// desktop notification.
#[test]
fn host_events_map_from_terminal_events() {
    use crate::event::{EventListener, TerminalEvent, WindowId};

    let shared = Arc::new(SessionSharedState::default());
    let events = &shared.events;
    let listener = TerminalEventProxy::new(Arc::clone(&shared), 1, None);
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

/// In-flight lifecycle: CommandStarted sets the running block, CommandFinished
/// finalizes it in place; trust loss and exit clear it without appending a block.
#[test]
fn in_flight_block_lifecycle() {
    use std::time::SystemTime;

    use crate::event::{CommandCapture, CommandStart, EventListener, TerminalEvent, WindowId};

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

    let shared = Arc::new(SessionSharedState::default());
    let events = &shared.events;
    let in_flight = &shared.in_flight;
    let open_prompt = &shared.open_prompt;
    let proxy = TerminalEventProxy::new(Arc::clone(&shared), 1, None);
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

    use crate::event::{
        BlockEvent, CommandCapture, CommandStart, EventListener, TerminalEvent, WindowId,
    };
    use crate::ghostty::BlockHandle;

    let shared = Arc::new(SessionSharedState::default());
    let store = &shared.block_store;
    let proxy = TerminalEventProxy::new(Arc::clone(&shared), 1, None);
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
#[derive(Default)]
struct TestObserver {
    wakes: Arc<Mutex<Vec<SessionChange>>>,
    images: Arc<Mutex<HashMap<u32, ()>>>,
}
impl SessionObserver for TestObserver {
    fn graphics(&self, updates: UpdateQueues) {
        let mut images = self.images.lock();
        for (id, _) in updates.pending_images {
            images.insert(id, ());
        }
        for id in updates.remove_queue {
            images.remove(&(id.0 as u32));
        }
    }

    fn changed(&self, change: SessionChange) {
        self.wakes.lock().push(change);
    }
}

fn graphics_proxy(id: u64) -> (TerminalEventProxy, GraphicsProbes) {
    let shared = Arc::new(SessionSharedState::default());
    let observer = Arc::new(TestObserver::default());
    let proxy = TerminalEventProxy::new(shared.clone(), id, Some(observer.clone()));
    (
        proxy,
        GraphicsProbes {
            shared,
            wakes: observer.wakes.clone(),
            images: observer.images.clone(),
        },
    )
}

struct GraphicsProbes {
    shared: Arc<SessionSharedState>,
    wakes: Arc<Mutex<Vec<SessionChange>>>,
    images: Arc<Mutex<HashMap<u32, ()>>>,
}

fn rgba_update(route_id: usize, image_id: u32, w: usize, h: usize) -> TerminalEvent {
    use crate::graphics::{ColorType, GraphicData, GraphicId, UpdateQueues};

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
    use crate::event::{EventListener, WindowId};

    let (proxy, p) = graphics_proxy(4);
    let wid = WindowId::dummy();

    proxy.send_event(rgba_update(4, 7, 2, 2), wid);

    assert!(
        p.shared.events.lock().is_empty(),
        "graphics never enters the host queue"
    );
    assert!(p.images.lock().get(&7).is_some(), "generation installed");
    assert_eq!(
        *p.wakes.lock(),
        vec![SessionChange::Content],
        "one content wake"
    );

    // A cross-session route is dropped: no install, no wake.
    proxy.send_event(rgba_update(999, 8, 2, 2), wid);

    assert!(p.images.lock().get(&8).is_none(), "wrong route ignored");
    assert_eq!(p.wakes.lock().len(), 1, "no wake for wrong route");
}

/// Sustained output cannot grow an unbounded UI-facing queue. Each
/// read's staged block events flush on their damage wake, and the host-event queue is
/// never touched, so neither the staging buffer nor the host queue accumulates.
#[test]
fn sustained_output_does_not_grow_ui_queue() {
    use crate::event::{BlockEvent, EventListener, WindowId};
    use crate::ghostty::BlockHandle;

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
            p.shared.staged_blocks.lock().is_empty(),
            "staging bounded to one read"
        );
    }

    assert!(
        p.shared.events.lock().is_empty(),
        "host queue never grew from graphics/block events"
    );

    // The live generation is a single replaced entry, not 1000 accumulated ones.
    assert_eq!(p.images.lock().len(), 1, "one live generation, replaced");
}

/// On the UI wake, active (live generation) and frozen (block-store
/// history) image state are both present — the read installed the generation
/// before flushing the block batch that froze the same content.
#[test]
fn active_and_frozen_state_coherent_at_wake() {
    use crate::event::{BlockEvent, EventListener, WindowId};
    use crate::ghostty::BlockHandle;

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
    assert!(p.shared.block_store.lock().items().is_empty());

    proxy.send_event(TerminalEvent::TerminalDamaged(1), wid);

    // At the wake both sides are coherent: live generation present AND frozen row
    // committed to history.
    assert!(
        p.images.lock().get(&42).is_some(),
        "live generation present"
    );
    assert_eq!(
        p.shared.block_store.lock().items().len(),
        1,
        "frozen history committed by the same wake"
    );
    assert!(p.wakes.lock().contains(&SessionChange::Content));
}

#[test]
fn final_damage_callback_observes_published_blocks_after_graphics() {
    use crate::event::{BlockEvent, EventListener, WindowId};
    use crate::ghostty::BlockHandle;

    struct PublicationObserver {
        shared: Arc<SessionSharedState>,
        steps: Mutex<Vec<(&'static str, usize)>>,
    }

    impl SessionObserver for PublicationObserver {
        fn graphics(&self, _: UpdateQueues) {
            self.steps.lock().push(("graphics", 0));
        }

        fn blocks(&self, _: &[BlockEvent]) {
            self.steps.lock().push(("blocks", 0));
        }

        fn changed(&self, _: SessionChange) {
            self.steps
                .lock()
                .push(("wake", self.shared.block_store.lock().items().len()));
        }
    }

    let shared = Arc::new(SessionSharedState::default());
    let observer = Arc::new(PublicationObserver {
        shared: shared.clone(),
        steps: Mutex::default(),
    });
    let proxy = TerminalEventProxy::new(shared, 1, Some(observer.clone()));
    let window = WindowId::dummy();
    proxy.send_event(
        TerminalEvent::BlockBatch(vec![BlockEvent::EngineBlock {
            seq: 1,
            handle: BlockHandle {
                id: 1,
                generation: 1,
            },
            rows: 2,
        }]),
        window,
    );
    proxy.send_event(rgba_update(1, 42, 2, 2), window);
    proxy.send_event(TerminalEvent::TerminalDamaged(1), window);

    assert_eq!(
        *observer.steps.lock(),
        [("graphics", 0), ("wake", 0), ("blocks", 0), ("wake", 1),]
    );
}
