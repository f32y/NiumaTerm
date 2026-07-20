#![cfg(windows)]

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nmt_platform::ProcessReadWrite;
use nmt_remote_session_hub::ipc::{
    DEFAULT_MAILBOX_CAPACITY, HubRequest, HubResponse, SharedMemoryEndpoint,
};
use nmt_remote_session_hub::{HubClient, SessionOptions};

#[test]
fn child_hub_carries_conpty_io_through_shared_memory() {
    let mut ipc = SharedMemoryEndpoint::create_parent(DEFAULT_MAILBOX_CAPACITY)
        .expect("create shared-memory transport");
    let mut child = ChildGuard::new(
        Command::new(env!("CARGO_BIN_EXE_SessionHub"))
            .arg(ipc.os_id())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start Hub child"),
    );

    send(
        &mut ipc,
        HubRequest::Open {
            request_id: 1,
            options: SessionOptions {
                shell: "powershell.exe".to_owned(),
                args: vec!["-NoLogo".to_owned(), "-NoProfile".to_owned()],
                ..SessionOptions::default()
            },
        },
    );
    let session_id = loop {
        if let HubResponse::Opened {
            request_id: 1,
            session_id,
        } = recv(&mut ipc)
        {
            break session_id;
        }
    };

    send(
        &mut ipc,
        HubRequest::Attach {
            request_id: 2,
            session_id,
        },
    );
    loop {
        if matches!(recv(&mut ipc), HubResponse::Snapshot { request_id: 2, .. }) {
            break;
        }
    }

    send(
        &mut ipc,
        HubRequest::Input {
            session_id,
            data: b"Write-Output NMT_SHARED_MEMORY_READY\r".to_vec(),
        },
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut output = Vec::new();
    while Instant::now() < deadline {
        if let HubResponse::Output { data, .. } = recv(&mut ipc) {
            output.extend_from_slice(&data);
            if output
                .windows(b"NMT_SHARED_MEMORY_READY".len())
                .any(|window| window == b"NMT_SHARED_MEMORY_READY")
            {
                break;
            }
        }
    }
    assert!(
        output
            .windows(b"NMT_SHARED_MEMORY_READY".len())
            .any(|window| window == b"NMT_SHARED_MEMORY_READY"),
        "ConPTY output did not cross shared memory"
    );

    send(
        &mut ipc,
        HubRequest::Kill {
            request_id: 3,
            session_id,
        },
    );
    loop {
        if matches!(recv(&mut ipc), HubResponse::Ack { request_id: 3 }) {
            break;
        }
    }
    send(&mut ipc, HubRequest::Shutdown);

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if child.child.try_wait().expect("query Hub child").is_some() {
            child.finished = true;
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("Hub child did not stop");
}

#[test]
fn remote_pty_adapter_carries_terminal_io() {
    let client = HubClient::spawn(Path::new(env!("CARGO_BIN_EXE_SessionHub")))
        .expect("start SessionHub client");
    let mut pty = client
        .open(SessionOptions {
            shell: "powershell.exe".to_owned(),
            args: vec!["-NoLogo".to_owned(), "-NoProfile".to_owned()],
            ..SessionOptions::default()
        })
        .expect("open remote PTY");
    pty.writer()
        .write_all(b"Write-Output NMT_REMOTE_PTY_READY\r")
        .expect("write terminal input");

    let marker = b"NMT_REMOTE_PTY_READY";
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut output = Vec::new();
    let mut buffer = [0; 4096];
    while Instant::now() < deadline {
        match pty.reader().read(&mut buffer) {
            Ok(count) if count > 0 => output.extend_from_slice(&buffer[..count]),
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("read remote PTY: {error}"),
        }
        if output.windows(marker.len()).any(|window| window == marker) {
            return;
        }
    }
    panic!("RemotePty output did not cross SessionHub");
}

fn send(ipc: &mut SharedMemoryEndpoint, request: HubRequest) {
    ipc.send(&request.encode().unwrap(), Duration::from_secs(5))
        .expect("send request");
}

fn recv(ipc: &mut SharedMemoryEndpoint) -> HubResponse {
    let bytes = ipc
        .recv(Duration::from_secs(5))
        .expect("receive response")
        .expect("Hub response timed out");
    let response = HubResponse::decode(&bytes).expect("decode response");
    if let HubResponse::Error { message, .. } = &response {
        panic!("Hub returned error: {message}");
    }
    response
}

struct ChildGuard {
    child: Child,
    finished: bool,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self {
            child,
            finished: false,
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
