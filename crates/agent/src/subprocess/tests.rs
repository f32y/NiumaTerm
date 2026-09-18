use std::io::Cursor;
use std::process::Command;
use std::sync::Barrier;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use nmt_platform::process::hidden_command;
use serde_json::{Value, json};
use tokio::io::BufReader;

use crate::subprocess::input::{InputClosed, InputQueue};
use crate::subprocess::requests::DeadlineTimer;
use crate::subprocess::{JsonLineProcess, OUTPUT_FAILURE_METHOD, read_messages};

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

    nmt_runtime::handle()
        .block_on(process.shutdown(Duration::from_secs(1), true))
        .unwrap();
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

    nmt_runtime::handle()
        .block_on(process.shutdown(Duration::from_secs(5), false))
        .unwrap();
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

    nmt_runtime::handle()
        .block_on(process.shutdown(Duration::from_millis(20), true))
        .unwrap();

    closed_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    nmt_runtime::handle()
        .block_on(process.shutdown(Duration::from_secs(1), false))
        .unwrap();
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

    nmt_runtime::handle()
        .block_on(process.shutdown(Duration::from_secs(5), false))
        .unwrap();

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

    nmt_runtime::handle()
        .block_on(process.shutdown(Duration::from_secs(1), false))
        .expect("exited process should be observable");
}

#[test]
fn large_history_reply_preserves_following_messages() {
    let history = "x".repeat(9 * 1024 * 1024);

    let input = format!(
        "{}\n{{\"next\":true}}\n",
        json!({"result":{"history":history}})
    );

    let mut reader = BufReader::new(Cursor::new(input));
    let mut messages = Vec::new();

    block_on(read_messages(&mut reader, "Test", |message| {
        messages.push(message)
    }))
    .unwrap();

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["result"]["history"].as_str().unwrap(), history);
    assert_eq!(messages[1], json!({"next":true}));
}

#[test]
fn startup_notices_bom_and_blank_lines_preserve_protocol_objects() {
    let mut reader = Cursor::new(b"\xef\xbb\xbfStarting agent\r\n[WARN] startup notice\n\n{\"ready\":true}\r\n  \n{\"done\":true}");
    let mut messages = Vec::new();

    block_on(read_messages(&mut reader, "Test", |message| {
        messages.push(message)
    }))
    .unwrap();

    assert_eq!(messages, [json!({"ready":true}), json!({"done":true})]);
}

#[test]
fn malformed_protocol_stops_before_later_messages_without_exposing_input() {
    let mut reader =
        Cursor::new(b"{\"ready\":true}\n{\"token\":\"private-value\",\n{\"late\":true}\n");

    let mut messages = Vec::new();

    let error = block_on(read_messages(&mut reader, "Test", |message| {
        messages.push(message)
    }))
    .unwrap_err();

    assert_eq!(messages, [json!({"ready":true})]);
    assert!(error.contains("JSON is invalid"));
    assert!(!error.contains("private-value"));
    assert!(!error.contains("token"));
}

#[test]
fn malformed_startup_json_invalid_utf8_and_non_objects_fail() {
    for bytes in [
        b"{broken\n".as_slice(),
        b"[1,]\n",
        b"\xff\n",
        b"null\n",
        b"[]\n",
        b"42\n",
        b"\"text\"\n",
        b"{}\nlate notice\n",
    ] {
        assert!(block_on(read_messages(&mut Cursor::new(bytes), "Test", |_| {})).is_err());
    }
}

#[test]
fn long_startup_notices_preserve_the_first_protocol_message() {
    let input = format!(
        "{}{}\n{{\"ready\":true}}",
        "notice\n".repeat(32),
        "n".repeat(128 * 1024)
    );

    let mut messages = Vec::new();

    block_on(read_messages(&mut Cursor::new(input), "Test", |message| {
        messages.push(message)
    }))
    .unwrap();

    assert_eq!(messages, [json!({"ready":true})]);
}

#[test]
fn large_input_and_queued_burst_preserve_order_while_a_write_is_active() {
    let (queue, receiver) = InputQueue::new();

    queue.submit(vec![json!("active")]).unwrap();

    let writing = block_on(receiver.recv()).unwrap();

    queue
        .submit(vec![json!("x".repeat(33 * 1024 * 1024))])
        .unwrap();

    for index in 0..2048 {
        queue.submit(vec![json!(index)]).unwrap();
    }

    assert_eq!(writing.messages, [json!("active")]);
    assert_eq!(
        block_on(receiver.recv()).unwrap().messages[0]
            .as_str()
            .unwrap()
            .len(),
        33 * 1024 * 1024
    );

    for index in 0..2048 {
        assert_eq!(block_on(receiver.recv()).unwrap().messages, [json!(index)]);
    }
}

#[test]
fn cancelled_payloads_are_removed_while_a_write_is_active() {
    let (queue, receiver) = InputQueue::new();

    queue.submit(vec![json!("active")]).unwrap();

    let writing = block_on(receiver.recv()).unwrap();

    for _ in 0..128 {
        let ticket = queue.submit_tracked(vec![json!("queued")]).unwrap();

        assert!(ticket.cancel());
        assert!(queue.is_empty());
    }

    assert_eq!(writing.messages, [json!("active")]);
}

#[test]
fn cancellation_and_writer_start_have_exactly_one_winner() {
    for _ in 0..128 {
        let (queue, receiver) = InputQueue::new();
        let ticket = queue.submit_tracked(vec![json!("message")]).unwrap();
        let barrier = Barrier::new(2);

        thread::scope(|scope| {
            let cancel = scope.spawn(|| {
                barrier.wait();

                ticket.cancel()
            });

            barrier.wait();

            let writing = receiver.try_recv().ok();

            assert_ne!(cancel.join().unwrap(), writing.is_some());
        });

        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn sender_close_drains_accepted_input_then_wakes_the_receiver() {
    let (queue, receiver) = InputQueue::new();

    queue.submit(vec![json!(1)]).unwrap();

    drop(queue);

    assert_eq!(block_on(receiver.recv()).unwrap().messages, [json!(1)]);
    assert!(block_on(receiver.recv()).is_none());
}

#[test]
fn cancellation_wins_before_start_and_cannot_split_a_started_batch() {
    let (queue, receiver) = InputQueue::new();

    let ticket = queue
        .submit_tracked(vec![json!("settings"), json!({"type":"user"})])
        .unwrap();

    assert!(ticket.is_batch());
    assert!(ticket.cancel());
    assert!(ticket.cancel());
    assert!(receiver.try_recv().is_err());

    let ticket = queue
        .submit_tracked(vec![json!("settings"), json!({"type":"user"})])
        .unwrap();

    let writing = block_on(receiver.recv()).unwrap();

    assert!(!ticket.cancel());
    assert_eq!(writing.messages.len(), 2);
}

#[test]
fn submission_moves_strings_and_disconnect_cancels_pending_input() {
    let (queue, receiver) = InputQueue::new();
    let payload = "original allocation".to_string();
    let address = payload.as_ptr();

    queue.submit(vec![Value::String(payload)]).unwrap();

    let input = block_on(receiver.recv()).unwrap();

    assert_eq!(input.messages[0].as_str().unwrap().as_ptr(), address);

    let ticket = queue
        .submit_tracked(vec![json!("cancel on close")])
        .unwrap();

    drop(receiver);

    assert!(ticket.is_cancelled());
    assert_eq!(queue.submit(vec![json!("closed")]), Err(InputClosed));
}

#[test]
fn idle_timer_rearms_for_earlier_work_and_stops_with_its_owner() {
    let (tx, rx) = channel();

    let timer = DeadlineTimer::new(move || {
        let _ = tx.send(());
    });

    assert_eq!(
        rx.recv_timeout(Duration::from_millis(20)),
        Err(RecvTimeoutError::Timeout)
    );

    timer
        .handle()
        .set(Some(Instant::now() + Duration::from_secs(30)));

    timer.handle().set(Some(Instant::now()));

    rx.recv_timeout(Duration::from_secs(2)).unwrap();

    assert_eq!(
        rx.recv_timeout(Duration::from_millis(20)),
        Err(RecvTimeoutError::Timeout)
    );

    timer
        .handle()
        .set(Some(Instant::now() + Duration::from_secs(30)));

    timer.handle().set(None);

    assert_eq!(
        rx.recv_timeout(Duration::from_millis(20)),
        Err(RecvTimeoutError::Timeout)
    );

    drop(timer);

    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2)),
        Err(RecvTimeoutError::Disconnected)
    );
}
