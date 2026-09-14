#![cfg(windows)]

use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use nmt_config::CursorShape;
use nmt_config::colors::Colors;
use nmt_platform::windows::powershell::{encode_command, preferred_shell};
use nmt_platform::{PtyOptions, WinsizeBuilder, create_managed_pty_with_env};
use nmt_terminal::event::{Msg, VoidListener};
use nmt_terminal::ghostty::GhosttyTerminal;
use nmt_terminal::pty_pipe::{SessionHandles, SessionOptions, start_session};
use nmt_terminal::render_buffer::RenderBuffer;
use nmt_terminal::session::request::CheckpointRequest;

const PROMPT: &str = "NMT> ";
const TIMEOUT: Duration = Duration::from_secs(10);

fn powershell_session(extra_args: &[&str], setup: &str) -> SessionHandles {
    let script = format!(
        r#"$ErrorActionPreference = 'Stop'
Import-Module PSReadLine
Set-PSReadLineOption -HistorySaveStyle SaveNothing
function global:prompt {{ 'NMT> ' }}
[Console]::Clear()
{setup}
"#,
    );

    let mut args: Vec<String> = extra_args.iter().map(|arg| (*arg).into()).collect();

    args.extend([
        "-NoLogo".into(),
        "-NoExit".into(),
        "-EncodedCommand".into(),
        encode_command(&script),
    ]);

    let pty = create_managed_pty_with_env(PtyOptions {
        shell: preferred_shell(),
        args: &args,
        working_directory: None,
        columns: 80,
        rows: 24,
        environment_overrides: &[("POWERSHELL_UPDATECHECK".into(), "Off".into())],
        starting_title: None,
        bootstrap: None,
    })
    .expect("start PowerShell under bundled ConPTY");

    start_session(
        pty,
        VoidListener {},
        SessionOptions {
            cols: 80,
            rows: 24,
            route_id: 0,
            colors: Colors::default(),
            cursor_shape: CursorShape::default(),
            scrollback_lines: 1000,
            engine_blocks: false,
            terminal_responses: true,
            output_sink: None,
        },
    )
    .expect("start terminal session")
}

fn screen_text(snapshot: &RenderBuffer) -> String {
    (0..snapshot.rows())
        .map(|y| {
            (0..snapshot.cols())
                .map(|x| match snapshot.cell(x, y).c() {
                    '\0' => ' ',
                    c => c,
                })
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn wait_for(
    session: &SessionHandles,
    description: &str,
    condition: impl Fn(&RenderBuffer) -> bool,
) -> Arc<RenderBuffer> {
    let deadline = Instant::now() + TIMEOUT;

    loop {
        let snapshot = session.render_buffer.load();

        if condition(&snapshot) {
            return snapshot;
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}; cursor={:?}, visible={}\n{}",
            snapshot.cursor(),
            snapshot.cursor_visible(),
            screen_text(&snapshot),
        );

        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_input(session: &SessionHandles, expected: &str) -> Arc<RenderBuffer> {
    wait_for(
        session,
        &format!("visible input {expected:?}"),
        |snapshot| {
            let cursor = snapshot.cursor();

            let Ok(cursor_row) = usize::try_from(cursor.row.0) else {
                return false;
            };

            if !snapshot.cursor_visible() || cursor_row >= snapshot.rows() {
                return false;
            }

            let mut before_cursor = String::new();

            for row in 0..=cursor_row {
                let end = if row == cursor_row {
                    cursor.col.0.min(snapshot.cols())
                } else {
                    snapshot.cols()
                };

                for col in 0..end {
                    before_cursor.push(match snapshot.cell(col, row).c() {
                        '\0' => ' ',
                        c => c,
                    });
                }
            }

            before_cursor
                .rsplit_once(PROMPT)
                .is_some_and(|(_, input)| input == expected)
        },
    )
}

fn resize(session: &SessionHandles, cols: u16, rows: u16) {
    session
        .messenger
        .send(Msg::Resize(WinsizeBuilder {
            cols,
            rows,
            width: cols * 8,
            height: rows * 16,
        }))
        .unwrap();
}

#[test]
fn typing_past_right_edge_keeps_every_character_visible() {
    let session = powershell_session(
        &["-NoProfile"],
        "Set-PSReadLineOption -PredictionSource None",
    );

    wait_for_input(&session, "");

    session
        .messenger
        .send(Msg::Input(vec![b'x'; 120].into()))
        .unwrap();

    let snapshot = wait_for_input(&session, &"x".repeat(120));

    assert_eq!(screen_text(&snapshot).matches('x').count(), 120);
}

#[test]
fn per_keystroke_typing_during_resize_keeps_complete_input() {
    let session = powershell_session(
        &["-NoProfile"],
        "Set-PSReadLineOption -PredictionSource None",
    );

    wait_for_input(&session, "");

    let mut expected = String::new();

    for (cols, rows) in [(111, 42), (41, 12), (95, 30), (80, 24)] {
        resize(&session, cols, rows);

        for _ in 0..30 {
            session
                .messenger
                .send(Msg::Input(vec![b'x'].into()))
                .unwrap();

            expected.push('x');
        }

        let snapshot = wait_for_input(&session, &expected);

        assert_eq!(screen_text(&snapshot).matches('x').count(), expected.len());
    }
}

fn scrolled_history_session() -> SessionHandles {
    let session = powershell_session(
        &["-NoProfile"],
        "Set-PSReadLineOption -PredictionSource None; 1..40 | ForEach-Object { 'HISTORY{0:D2}' -f $_ }",
    );

    wait_for_input(&session, "");
    session.messenger.send(Msg::ScrollTo(0)).unwrap();

    wait_for(&session, "history viewport", |snapshot| {
        let scroll = snapshot.scrollbar();

        scroll.offset < scroll.total.saturating_sub(scroll.len)
    });

    session
}

#[test]
#[ignore = "Known resize/input displacement also occurs before input ordering; see docs/research/conpty-realign-input-desync.md"]
fn resize_while_scrolled_preserves_history_and_live_input() {
    let session = scrolled_history_session();

    resize(&session, 60, 20);

    wait_for(&session, "smaller viewport", |snapshot| {
        snapshot.cols() == 60 && snapshot.rows() == 20
    });

    resize(&session, 100, 30);

    wait_for(&session, "larger viewport", |snapshot| {
        snapshot.cols() == 100 && snapshot.rows() == 30
    });

    session.messenger.send(Msg::ScrollToEnd).unwrap();

    session
        .messenger
        .send(Msg::Input(b"echo NMT_VISIBLE".to_vec().into()))
        .unwrap();

    wait_for_input(&session, "echo NMT_VISIBLE");

    let (send, receive) = mpsc::channel();

    session
        .messenger
        .send(Msg::Checkpoint(CheckpointRequest(Box::new(
            move |result| {
                send.send(result).unwrap();
            },
        ))))
        .unwrap();

    let checkpoint = receive.recv_timeout(TIMEOUT).unwrap().unwrap();
    let mut engine = GhosttyTerminal::new(checkpoint.cols, checkpoint.rows, 1_000_000).unwrap();

    engine.write_vt(&checkpoint.vt);

    let text = engine.format_text(None, false, true).unwrap();

    for line in 1..=40 {
        let marker = format!("HISTORY{line:02}");

        assert_eq!(
            text.matches(&marker).count(),
            1,
            "{marker} lost or duplicated:\n{text}"
        );
    }
}

fn check_list_prediction_after_startup_resize(extra_args: &[&str]) {
    let session = powershell_session(
        extra_args,
        "Set-PSReadLineOption -PredictionSource History -PredictionViewStyle ListView",
    );

    resize(&session, 100, 30);
    wait_for_input(&session, "");

    session
        .messenger
        .send(Msg::Input(
            b"Write-Output NMT_PREDICTION_SEED\r".to_vec().into(),
        ))
        .unwrap();

    wait_for(&session, "history seed", |snapshot| {
        screen_text(snapshot).contains("NMT_PREDICTION_SEED")
    });

    wait_for_input(&session, "");

    session
        .messenger
        .send(Msg::Input(b"Write-O".to_vec().into()))
        .unwrap();

    wait_for_input(&session, "Write-O");

    wait_for(&session, "prediction list", |snapshot| {
        let cursor_row = snapshot.cursor().row.0.max(0) as usize;

        (cursor_row + 1..snapshot.rows()).any(|row| {
            let text: String = (0..snapshot.cols())
                .map(|col| snapshot.cell(col, row).c())
                .collect();

            text.contains("Write-Output NMT_PREDICTION_SEED")
        })
    });

    resize(&session, 80, 24);

    session
        .messenger
        .send(Msg::Input(b"utput".to_vec().into()))
        .unwrap();

    wait_for_input(&session, "Write-Output");
}

#[test]
fn list_prediction_survives_startup_resize() {
    check_list_prediction_after_startup_resize(&["-NoProfile"]);
}

#[test]
#[ignore = "Uses the local PowerShell profile"]
fn local_profile_list_prediction_survives_startup_resize() {
    check_list_prediction_after_startup_resize(&[]);
}

#[test]
#[ignore = "PSReadLine 2.4.5 can skip resize checks for 50 ms; see docs/research/conpty-resize-psreadline-render-check.md"]
fn immediate_input_after_shrink_and_grow_stays_with_prompt() {
    let session = scrolled_history_session();

    resize(&session, 60, 20);

    wait_for(&session, "smaller viewport", |snapshot| {
        snapshot.cols() == 60 && snapshot.rows() == 20
    });

    resize(&session, 100, 30);
    session.messenger.send(Msg::ScrollToEnd).unwrap();

    session
        .messenger
        .send(Msg::Input(b"echo NMT_VISIBLE".to_vec().into()))
        .unwrap();

    wait_for_input(&session, "echo NMT_VISIBLE");
}
