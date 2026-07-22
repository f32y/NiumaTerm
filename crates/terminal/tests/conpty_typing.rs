#![cfg(windows)]

use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::thread;
use std::time::Duration;

use nmt_config::colors::Colors;
use nmt_platform::{WinsizeBuilder, create_pty};
use nmt_terminal::event::{Msg, VoidListener, WindowId};
use nmt_terminal::ghostty::GhosttyTerminal;
use nmt_terminal::pty_pipe::PtyPipe;
use nmt_terminal::render_buffer::RenderBuffer;
use parking_lot::FairMutex;

fn snapshot_text(engine: &Arc<FairMutex<GhosttyTerminal>>) -> Vec<String> {
    let mut e = engine.lock();
    let snap = e.snapshot().expect("snapshot");
    let mut out = vec![String::new(); snap.rows()];
    for (y, out_row) in out.iter_mut().enumerate() {
        *out_row = (0..snap.cols())
            .map(|x| match snap.cell(x, y).c() {
                '\0' => ' ',
                c => c,
            })
            .collect::<String>()
            .trim_end()
            .to_string();
    }
    out
}

#[test]
fn typing_past_right_edge_does_not_duplicate() {
    let cols: u16 = 80;
    let rows: u16 = 24;

    let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(
        cols as usize,
        rows as usize,
    )));
    let vt_modes = Arc::new(AtomicU32::new(0));

    let pty = match create_pty("powershell.exe", vec![], &None, cols, rows) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("skipping: could not spawn powershell.exe: {e:?}");
            return;
        }
    };

    let machine = PtyPipe::new(
        render_buffer,
        vt_modes,
        pty,
        VoidListener {},
        WindowId::from(0),
        0,
        Colors::default(),
        1000,
        false,
    )
    .expect("machine");
    let engine = machine.engine();
    let sender = machine.channel();
    let _io = machine.spawn();

    // Let the prompt settle.
    thread::sleep(Duration::from_millis(2000));

    // Type 120 'x' (well past the 80-col right edge) so the input wraps.
    sender
        .send(Msg::Input(vec![b'x'; 120].into()))
        .expect("send input");
    thread::sleep(Duration::from_millis(2000));

    let text = snapshot_text(&engine);
    let total_x: usize = text.iter().map(|l| l.matches('x').count()).sum();

    // The input is 120 x's; with wrapping the grid holds ~120 of them. A
    // duplication bug balloons this (the report saw the prompt repeat ~28x).
    assert!(
        total_x <= 140,
        "x count ballooned to {total_x} (expected ~120) — prompt/input repaints \
         are duplicating. Grid:\n{}",
        text.join("\n")
    );
}

/// Same, but with a resize first (the real app starts at 80x24 then resizes to
/// the window). Mirrors `nmt_render_host_resize` -> `Msg::Resize`.
#[test]
fn typing_after_resize_does_not_duplicate() {
    let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(80, 24)));
    let vt_modes = Arc::new(AtomicU32::new(0));

    let pty = match create_pty("powershell.exe", vec![], &None, 80, 24) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("skipping: could not spawn powershell.exe: {e:?}");
            return;
        }
    };

    let machine = PtyPipe::new(
        render_buffer,
        vt_modes,
        pty,
        VoidListener {},
        WindowId::from(0),
        0,
        Colors::default(),
        1000,
        false,
    )
    .expect("machine");
    let engine = machine.engine();
    let sender = machine.channel();
    let _io = machine.spawn();

    thread::sleep(Duration::from_millis(2000));

    // Resize to a wider window, the way the app does on startup.
    sender
        .send(Msg::Resize(WinsizeBuilder {
            rows: 30,
            cols: 100,
            width: 800,
            height: 480,
        }))
        .expect("send resize");
    thread::sleep(Duration::from_millis(1000));

    sender
        .send(Msg::Input(vec![b'x'; 120].into()))
        .expect("send input");
    thread::sleep(Duration::from_millis(2000));

    let text = snapshot_text(&engine);
    let total_x: usize = text.iter().map(|l| l.matches('x').count()).sum();

    assert!(
        total_x <= 140,
        "x count ballooned to {total_x} after resize (expected ~120). Grid:\n{}",
        text.join("\n")
    );
}

/// Replicate terminal's per-keystroke input path exactly: 120 individual
/// `Msg::Input(1 x)`, each preceded by the scroll-to-bottom-if-scrolled check
/// from `write_input_to_session`. This is what the app does (one WM_CHAR per x)
/// and is the suspected source of the frantic-repeat bug.
#[test]
fn per_keystroke_typing_does_not_duplicate() {
    let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(80, 24)));
    let rb = Arc::clone(&render_buffer);
    let vt_modes = Arc::new(AtomicU32::new(0));

    let pty = match create_pty("powershell.exe", vec![], &None, 80, 24) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("skipping: could not spawn powershell.exe: {e:?}");
            return;
        }
    };

    let machine = PtyPipe::new(
        render_buffer,
        vt_modes,
        pty,
        VoidListener {},
        WindowId::from(0),
        0,
        Colors::default(),
        1000,
        false,
    )
    .expect("machine");
    let engine = machine.engine();
    let sender = machine.channel();
    let _io = machine.spawn();

    thread::sleep(Duration::from_millis(2000));

    for _ in 0..120 {
        let scrolled_up = {
            let sb = rb.lock().scrollbar();
            sb.offset < sb.total.saturating_sub(sb.len)
        };
        if scrolled_up {
            engine.lock().scroll_viewport_bottom();
        }
        sender.send(Msg::Input(vec![b'x'].into())).expect("send x");
        thread::sleep(Duration::from_millis(5));
    }
    thread::sleep(Duration::from_millis(2000));

    // Check both the engine snapshot and the render buffer (what the app reads).
    let engine_text = snapshot_text(&engine);
    let buf_text: Vec<String> = {
        let b = rb.lock();
        (0..b.rows())
            .map(|y| {
                (0..b.cols())
                    .map(|x| b.cell(x, y).c())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    };
    let engine_x: usize = engine_text.iter().map(|l| l.matches('x').count()).sum();
    let buf_x: usize = buf_text.iter().map(|l| l.matches('x').count()).sum();

    assert!(
        engine_x <= 140 && buf_x <= 140,
        "ballooned (per-keystroke): engine_x={engine_x} buf_x={buf_x}\nengine:\n{}\nbuf:\n{}",
        engine_text.join("\n"),
        buf_text.join("\n"),
    );
}

/// The app's exact combo: a resize (startup) then fast per-keystroke typing.
/// Suspected source of the engine accumulating PSReadLine repaints (3130 x's
/// observed in the live app for 120 typed).
#[test]
fn resize_then_fast_per_keystroke_does_not_duplicate() {
    let render_buffer = Arc::new(FairMutex::new(RenderBuffer::new(80, 24)));
    let vt_modes = Arc::new(AtomicU32::new(0));

    let pty = match create_pty("powershell.exe", vec![], &None, 80, 24) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("skipping: could not spawn powershell.exe: {e:?}");
            return;
        }
    };

    let machine = PtyPipe::new(
        render_buffer,
        vt_modes,
        pty,
        VoidListener {},
        WindowId::from(0),
        0,
        Colors::default(),
        1000,
        false,
    )
    .expect("machine");
    let engine = machine.engine();
    let sender = machine.channel();
    let _io = machine.spawn();

    thread::sleep(Duration::from_millis(2000));

    // Resize to the live app's observed size (111x42), the way startup does.
    sender
        .send(Msg::Resize(WinsizeBuilder {
            rows: 42,
            cols: 111,
            width: 111 * 12,
            height: 42 * 24,
        }))
        .expect("resize");
    thread::sleep(Duration::from_millis(800));

    // Fast individual keystrokes (no sleep), like SendKeys.
    for _ in 0..120 {
        sender.send(Msg::Input(vec![b'x'].into())).expect("x");
    }
    thread::sleep(Duration::from_millis(2500));

    let text = snapshot_text(&engine);
    let total_x: usize = text.iter().map(|l| l.matches('x').count()).sum();

    assert!(
        total_x <= 140,
        "x count ballooned to {total_x} (resize+fast per-keystroke, expected ~120). Grid:\n{}",
        text.join("\n")
    );
}
