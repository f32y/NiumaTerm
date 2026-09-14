use std::process::Command;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use nmt_platform::process::hidden_command;
use serde_json::json;

use crate::message_memory::OUTPUT_FAILURE_METHOD;
use crate::subprocess::JsonLineProcess;

#[test]
fn malformed_output_reports_failure_before_eof_and_stops_delivery() {
    let command = script(
        "[Console]::Out.WriteLine('Starting agent'); [Console]::Out.WriteLine('{\"ready\":true}'); [Console]::Out.WriteLine('{\"token\":\"private-value\",'); [Console]::Out.WriteLine('{\"late\":true}'); Start-Sleep -Seconds 30",
        "echo 'Starting agent'; echo '{\"ready\":true}'; echo '{\"token\":\"private-value\",'; echo '{\"late\":true}'; sleep 30",
    );

    let (tx, rx) = channel();
    let (closed_tx, closed_rx) = channel();

    let mut process = JsonLineProcess::spawn_with_stdout_closed(
        command,
        "malformed-output",
        "Test",
        move |message| {
            let _ = tx.send(message);
        },
        |_| {},
        move || {
            let _ = closed_tx.send(());
        },
    )
    .unwrap();

    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        json!({"ready":true})
    );

    let failure = rx.recv_timeout(Duration::from_secs(5)).unwrap();

    assert_eq!(failure["method"], OUTPUT_FAILURE_METHOD);

    let reason = failure["params"]["message"].as_str().unwrap();

    assert!(reason.contains("JSON is invalid"));
    assert!(!reason.contains("private-value"));

    closed_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    assert!(rx.try_recv().is_err());
    assert!(!process.has_stdin());

    process.shutdown(Duration::from_secs(1), true).unwrap();
}

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
fn long_stderr_lines_remain_complete_and_separate_from_protocol_output() {
    let command = script(
        "[Console]::Error.WriteLine(('x' * 131072)); [Console]::Error.WriteLine('next'); [Console]::Out.WriteLine('{\"ready\":true}')",
        "head -c 131072 /dev/zero | tr '\\000' x >&2; printf '\\nnext\\n' >&2; echo '{\"ready\":true}'",
    );

    let (output_tx, output_rx) = channel();
    let (stderr_tx, stderr_rx) = channel();

    let mut process = JsonLineProcess::spawn(
        command,
        "long-stderr",
        "Test",
        move |message| {
            let _ = output_tx.send(message);
        },
        move |line| {
            let _ = stderr_tx.send(line);
        },
    )
    .unwrap();

    assert_eq!(
        stderr_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        "x".repeat(128 * 1024)
    );
    assert_eq!(
        stderr_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        "next"
    );
    assert_eq!(
        output_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        json!({"ready":true})
    );

    process.shutdown(Duration::from_secs(5), false).unwrap();
}

#[test]
fn stalled_input_accepts_a_message_burst_without_closing_the_process() {
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

    process.try_write_line(large).unwrap();

    assert!(started.elapsed() < Duration::from_secs(1));

    for _ in 0..2048 {
        process.try_write_line(json!({"next":true})).unwrap();
    }

    assert!(process.has_stdin());
    assert!(closed_rx.try_recv().is_err());

    process.try_write_line(json!({"interrupt":true})).unwrap();

    process.shutdown(Duration::from_millis(20), true).unwrap();

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
        process.try_write_line(json!({"index":index})).unwrap();
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
