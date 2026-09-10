#[cfg(unix)]
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

#[cfg(unix)]
use nmt_config::colors::{ColorRgb, Colors, NamedColor};
use nmt_config::local_state::TabState;
use nmt_input::keyboard::ModifiersState;
use nmt_terminal::ghostty::GhosttyTerminal;
use nmt_terminal::render_buffer::RenderBuffer;
use nmt_terminal::selection::{Selection, SelectionType};
use nmt_terminal::terminal::Mode;
use nmt_terminal::terminal::pos::{Column, Line, Pos, Side};

use crate::session::TerminalSessionConfig;
use crate::surface::{
    SurfaceMouseButton, TerminalSurface, block_selection_range, mouse_button_code,
    mouse_motion_code, mouse_report_mods, paste_payload, selection_screen_range,
    tab_state_with_cwd,
};
#[cfg(unix)]
use crate::wake::WakeSender;

#[cfg(unix)]
#[test]
fn theme_switch_updates_idle_terminal_snapshot() {
    let (wake_tx, wake_rx) = mpsc::channel();
    let wake = WakeSender::from_fn(move |event| {
        let _ = wake_tx.send(event);
    });
    let surface = TerminalSurface::new(
        TerminalSessionConfig {
            shell: Some("/bin/cat".into()),
            cols: 20,
            rows: 3,
            ..Default::default()
        },
        1,
        Some(wake),
    )
    .unwrap();

    {
        let mut engine = surface.session.engine.lock();
        engine.write_vt(b"idle prompt");
        *surface.session.render_buffer.lock() = engine.snapshot().unwrap();
    }

    for (foreground, background) in [
        ([32, 30, 28], [250, 248, 245]),
        ([230, 228, 225], [25, 23, 21]),
    ] {
        let mut colors = Colors::default();
        let rgb = |[r, g, b]: [u8; 3]| ColorRgb { r, g, b }.to_arr();
        colors.foreground = rgb(foreground);
        colors.background.0 = rgb(background);
        while wake_rx.try_recv().is_ok() {}
        assert!(surface.set_theme_colors(&colors));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            wake_rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("idle theme refresh must wake the view");
            if surface.with_render_buffer(|buffer| {
                buffer.colors()[NamedColor::Foreground] == Some(rgb(foreground))
                    && buffer.colors()[NamedColor::Background] == Some(rgb(background))
            }) {
                break;
            }
        }

        surface.with_render_buffer(|buffer| {
            assert_eq!(
                buffer.colors()[NamedColor::Foreground],
                Some(rgb(foreground))
            );
            assert_eq!(
                buffer.colors()[NamedColor::Background],
                Some(rgb(background))
            );
        });
    }
}

#[test]
fn bad_shell_returns_error() {
    let config = TerminalSessionConfig {
        shell: Some("this-shell-does-not-exist-xyz.exe".to_string()),
        ..TerminalSessionConfig::default()
    };

    let err = TerminalSurface::new(config, 1, None)
        .err()
        .expect("bad shell must fail");

    assert!(err.contains("PtySpawn"));
}

#[test]
fn frozen_click_selection_expands_words_and_wrapped_lines() {
    let mut terminal = GhosttyTerminal::new(8, 4, 100).unwrap();
    terminal.write_vt(b"foo bar\r\nabcdefghijk");
    let handle = terminal.finish_block().unwrap().expect("block created");
    let block = terminal.block_acquire(handle).expect("block acquired");
    let palette = terminal.color_palette();

    assert_eq!(
        block_selection_range(&block, &palette, 0, 5, SelectionType::Semantic),
        Some(((0, 4), (0, 6)))
    );
    assert_eq!(
        block_selection_range(&block, &palette, 2, 1, SelectionType::Semantic),
        Some(((1, 0), (2, 2)))
    );
    assert_eq!(
        block_selection_range(&block, &palette, 2, 1, SelectionType::Lines),
        Some(((1, 0), (2, 7)))
    );
}

#[test]
fn screen_word_selection_searches_the_visible_row_before_rebasing() {
    let mut terminal = GhosttyTerminal::new(24, 2, 100).unwrap();
    terminal.write_vt(b"pipelines.universal\r\npi");
    let mut buf = RenderBuffer::new(24, 2);
    terminal.snapshot_into(&mut buf).unwrap();
    let selection = Selection::new(
        SelectionType::Semantic,
        Pos::new(Line(1), Column(1)),
        Side::Left,
    );

    let range = selection_screen_range(&selection, &buf, 1).unwrap();

    assert_eq!(range.start, Pos::new(Line(1), Column(0)));
    assert_eq!(range.end, Pos::new(Line(1), Column(8)));
}

#[test]
fn osc7_pwd_normalizes_to_filesystem_path() {
    use crate::surface::normalize_osc7_pwd;
    assert_eq!(
        normalize_osc7_pwd("file:///C:/Projects/example"),
        "C:/Projects/example"
    );
    assert_eq!(normalize_osc7_pwd("file://host/home/u"), "/home/u");
    assert_eq!(normalize_osc7_pwd(r"C:\plain\path"), r"C:\plain\path");
    assert_eq!(normalize_osc7_pwd("/unix/path"), "/unix/path");
}

#[test]
fn tab_state_uses_last_reported_cwd() {
    let launch = TabState {
        name: None,
        user_named: false,
        shell: Some("pwsh.exe".into()),
        args: vec!["-NoLogo".into()],
        cwd: Some("C:/old".into()),
        agent: None,
        agent_profile: None,
        panes: None,
    };

    let state = tab_state_with_cwd(&launch, Some("C:/new".into()));

    assert_eq!(state.shell, launch.shell);
    assert_eq!(state.args, launch.args);
    assert_eq!(state.cwd.as_deref(), Some("C:/new"));
}

#[test]
fn paste_payload_normalizes_and_guards() {
    assert_eq!(paste_payload("", false), None);
    assert_eq!(
        paste_payload("a\r\nb\nc", false).unwrap(),
        b"a\rb\rc".to_vec()
    );
    assert_eq!(
        paste_payload("ls\x1b[201~rm", true).unwrap(),
        b"\x1b[200~lsrm\x1b[201~".to_vec()
    );
}

#[test]
fn mouse_helpers_match_xterm_codes() {
    assert_eq!(mouse_button_code(SurfaceMouseButton::Left), Some(0));
    assert_eq!(mouse_button_code(SurfaceMouseButton::Middle), Some(1));
    assert_eq!(mouse_button_code(SurfaceMouseButton::Right), Some(2));
    assert_eq!(
        mouse_report_mods(ModifiersState::SHIFT | ModifiersState::CONTROL),
        20
    );
    assert_eq!(mouse_report_mods(ModifiersState::ALT), 8);
    assert_eq!(mouse_motion_code(Mode::MOUSE_MOTION, None), Some(35));
    assert_eq!(
        mouse_motion_code(Mode::MOUSE_DRAG, Some(SurfaceMouseButton::Right)),
        Some(34)
    );
}
