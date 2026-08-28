use std::process::Command;
use std::sync::mpsc::channel;
use std::time::Duration;

use crate::subprocess::JsonLineProcess;

#[test]
fn stdout_close_callback_follows_the_last_json_message() {
    let mut command = Command::new("powershell.exe");
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "[Console]::Out.WriteLine('{\"ready\":true}')",
    ]);
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
