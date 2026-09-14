// Drives a live PowerShell 7 with PSReadLine, so the whole file is Windows-only.
#![cfg(windows)]
#![cfg(windows)]

use std::path::Path;
use std::thread;
use std::time::Duration;

use nmt_config::active_colors;
use nmt_platform::powershell::encode_command;

use crate::session::vtebench_tests::{block_texts, screen_text, wait_for};
use crate::session::{HostEvent, TerminalSession, TerminalSessionConfig};

fn list_view_session(editor_setup: &str) -> (TerminalSession, Vec<HostEvent>) {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/windows/pwsh-integration.ps1");

    let startup = format!(
        "Import-Module PSReadLine -ErrorAction Stop; \
         Set-PSReadLineOption -HistorySaveStyle SaveNothing -PredictionSource History -PredictionViewStyle ListView; \
         [Microsoft.PowerShell.PSConsoleReadLine]::ClearHistory(); \
         [Microsoft.PowerShell.PSConsoleReadLine]::AddToHistory('ls Cargo.toml'); \
         {editor_setup}; function global:prompt {{ 'PS> ' }}; . '{}'",
        script.to_string_lossy().replace('\'', "''")
    );

    let config = TerminalSessionConfig {
        shell: Some("pwsh.exe".into()),
        args: vec![
            "-NoLogo".into(),
            "-NoProfile".into(),
            "-NoExit".into(),
            "-EncodedCommand".into(),
            encode_command(&startup),
        ],
        working_dir: Some(env!("CARGO_MANIFEST_DIR").into()),
        cols: 100,
        rows: 30,
        manage_process_tree: true,
        ..TerminalSessionConfig::default()
    };

    let session = TerminalSession::new(&config, 1, active_colors(), None)
        .expect("PowerShell 7 with PSReadLine ListView is required");

    let mut events = Vec::new();

    assert!(
        wait_for(&session, &mut events, Duration::from_secs(20), |events| {
            events.contains(&HostEvent::PromptBoundaryTrusted(true))
        }),
        "integration did not become ready: {}",
        screen_text(&session)
    );

    (session, events)
}

fn finish_input(session: &TerminalSession, events: &mut Vec<HostEvent>) {
    events.clear();
    session.write_input(b"\r");

    assert!(
        wait_for(session, events, Duration::from_secs(15), |events| {
            events
                .iter()
                .any(|event| matches!(event, HostEvent::CommandFinished { .. }))
                && events.contains(&HostEvent::PromptStarted)
        }),
        "command did not finish: {events:?}\nscreen: {}",
        screen_text(session)
    );
}

#[test]
fn list_view_keeps_three_ls_results_after_prompt_clears() {
    let (session, mut events) = list_view_session("");

    for count in 1..=3 {
        session.write_input(b"l");

        thread::sleep(Duration::from_millis(100));

        session.write_input(b"s");

        assert!(
            wait_for(&session, &mut events, Duration::from_secs(5), |_| {
                screen_text(&session).contains("ls Cargo.toml")
            }),
            "history prediction did not appear: {}",
            screen_text(&session)
        );

        finish_input(&session, &mut events);

        let blocks = block_texts(&session);

        assert_eq!(blocks.len(), count, "{blocks:?}");

        for (command, text) in blocks {
            assert_eq!(command.as_deref(), Some("ls"));
            assert!(
                text.contains("Cargo.toml"),
                "directory output missing: {text}"
            );
        }
    }

    for input in [b"\r".as_slice(), b"abandoned\x03"] {
        events.clear();
        session.write_input(input);

        assert!(wait_for(
            &session,
            &mut events,
            Duration::from_secs(5),
            |events| { events.contains(&HostEvent::PromptStarted) }
        ));
        assert!(!events.iter().any(|event| matches!(
            event,
            HostEvent::CommandStarted | HostEvent::CommandFinished { .. }
        )));
        assert_eq!(block_texts(&session).len(), 3);
    }
}

#[test]
fn psreadline_preserves_long_unicode_and_multiline_submissions() {
    let command = format!("Write-Output '中文🚀'\nWrite-Output '{}'", "z".repeat(900));

    // Insert through the editor API so console key translation cannot turn a
    // literal LF into a cursor movement while constructing the multiline input.
    let editor_setup = format!(
        "Set-PSReadLineKeyHandler -Chord Ctrl+g -ScriptBlock {{ \
         [Microsoft.PowerShell.PSConsoleReadLine]::Insert('{}') }}",
        command.replace('\'', "''")
    );

    let (session, mut events) = list_view_session(&editor_setup);

    session.write_input(b"\x07");

    thread::sleep(Duration::from_millis(500));
    finish_input(&session, &mut events);

    let blocks = block_texts(&session);

    assert_eq!(blocks.len(), 1, "{blocks:?}");
    assert_eq!(blocks[0].0.as_deref(), Some(command.as_str()));
    assert!(blocks[0].1.contains("中文🚀"), "{}", blocks[0].1);
    assert!(blocks[0].1.contains(&"z".repeat(80)), "{}", blocks[0].1);
}
