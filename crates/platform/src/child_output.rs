use std::io;
use std::process::{Command, Output};

use tokio::io::AsyncReadExt as _;

use crate::process::{PipedChild, spawn_piped};

/// Run `command` to completion and capture both output streams. Both pipes
/// are drained while the child runs, so a verbose child cannot block on a full
/// pipe; like `Command::output`, a descendant holding a pipe open delays the
/// result until it exits. The child reads no input.
pub async fn output(command: Command) -> io::Result<Output> {
    let PipedChild {
        mut child,
        stdin,
        mut stdout,
        mut stderr,
    } = spawn_piped(command)?;

    drop(stdin);

    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();

    let (stdout_read, stderr_read, status) = tokio::join!(
        stdout.read_to_end(&mut stdout_bytes),
        stderr.read_to_end(&mut stderr_bytes),
        child.wait(),
    );

    stdout_read?;
    stderr_read?;

    Ok(Output {
        status: status?,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
    })
}
