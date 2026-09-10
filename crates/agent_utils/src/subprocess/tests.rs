use std::process::Command;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use nmt_platform::process::hidden_command;
use serde_json::json;

use crate::subprocess::{InputClass, JsonLineProcess};

fn script(windows: &str, unix: &str) -> Command {
    #[cfg(windows)]
    {
        let _ = unix;
        let mut command = hidden_command("powershell.exe");
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            windows,
        ]);
        command
    }
    #[cfg(unix)]
    {
        let _ = windows;
        let mut command = hidden_command("/bin/sh");
        command.args(["-c", unix]);
        command
    }
}

#[test]
fn stalled_input_rejects_normal_overflow_without_closing_the_process() {
    let command = script(
        "[Console]::Out.WriteLine('{\"ready\":true}'); Start-Sleep -Seconds 30",
        "echo '{\"ready\":true}'; sleep 30",
    );
    let (tx, rx) = channel();
    let (closed_tx, closed_rx) = channel();
    let mut process = JsonLineProcess::spawn_with_stdout_closed(
        command,
        "stalled-input",
        "Test",
        move |value| {
            let _ = tx.send(value);
        },
        |_| {},
        move || {
            let _ = closed_tx.send(());
        },
    )
    .unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let large = json!({"text": "x".repeat(1024 * 1024)});
    let started = Instant::now();
    process.try_write_line(large, InputClass::Normal).unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    let mut rejected = false;
    for _ in 0..65 {
        if process
            .try_write_line(json!({"next":true}), InputClass::Normal)
            .is_err()
        {
            rejected = true;
            break;
        }
    }
    assert!(rejected);
    assert!(process.has_stdin());
    assert!(closed_rx.try_recv().is_err());
    process
        .try_write_line(json!({"interrupt":true}), InputClass::Control)
        .unwrap();
    process.shutdown(Duration::from_millis(20), true).unwrap();
    closed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    process.shutdown(Duration::from_secs(1), false).unwrap();
}

#[test]
fn exhausting_required_control_capacity_reports_process_exit() {
    let command = script(
        "[Console]::Out.WriteLine('{\"ready\":true}'); Start-Sleep -Seconds 30",
        "echo '{\"ready\":true}'; sleep 30",
    );
    let (tx, rx) = channel();
    let (closed_tx, closed_rx) = channel();
    let mut process = JsonLineProcess::spawn_with_stdout_closed(
        command,
        "blocked-controls",
        "Test",
        move |value| {
            let _ = tx.send(value);
        },
        |_| {},
        move || {
            let _ = closed_tx.send(());
        },
    )
    .unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    process
        .try_write_line(json!({"text":"x".repeat(1024 * 1024)}), InputClass::Normal)
        .unwrap();
    let mut rejected = false;
    for _ in 0..65 {
        if process.write_line(json!({"control":true})).is_err() {
            rejected = true;
            break;
        }
    }
    assert!(rejected);
    assert!(!process.has_stdin());
    closed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    process.shutdown(Duration::from_secs(1), false).unwrap();
}

#[test]
fn shutdown_drains_accepted_messages_in_order() {
    let command = script(
        "while ($null -ne ($line = [Console]::ReadLine())) { [Console]::Out.WriteLine($line) }",
        "while IFS= read -r line; do printf '%s\\n' \"$line\"; done",
    );
    let (tx, rx) = channel();
    let mut process = JsonLineProcess::spawn(
        command,
        "ordered-input",
        "Test",
        move |value| {
            let _ = tx.send(value);
        },
        |_| {},
    )
    .unwrap();
    for index in 0..20 {
        process
            .try_write_line(json!({"index":index}), InputClass::Normal)
            .unwrap();
    }
    process.shutdown(Duration::from_secs(5), false).unwrap();
    for index in 0..20 {
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap()["index"],
            index
        );
    }
}

#[test]
fn stdout_close_callback_follows_the_last_json_message() {
    // Built the way production callers do: `JsonLineProcess` contains the
    // child it spawns, which on Unix requires a command that leads its own
    // process group.
    #[cfg(windows)]
    let command = {
        let mut command = hidden_command("powershell.exe");
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::Out.WriteLine('{\"ready\":true}')",
        ]);
        command
    };
    #[cfg(unix)]
    let command = {
        let mut command = hidden_command("/bin/sh");
        command.args(["-c", "echo '{\"ready\":true}'"]);
        command
    };
    let (message_tx, message_rx) = channel();
    let (closed_tx, closed_rx) = channel();
    let mut process = JsonLineProcess::spawn_with_stdout_closed(
        command,
        "test-json-process",
        "Test",
        move |message| {
            let _ = message_tx.send(message);
        },
        |_| {},
        move || {
            let _ = closed_tx.send(());
        },
    )
    .expect("test process should start");

    assert_eq!(
        message_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("json message")["ready"],
        true
    );
    closed_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("stdout close callback");
    process
        .shutdown(Duration::from_secs(1), false)
        .expect("exited process should be observable");
}
