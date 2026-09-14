#![cfg(windows)]

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use nmt_platform::{EventedPty, Interest, Poll, ProcessReadWrite, Token, Waker, WinsizeBuilder};
use nmt_remote_net::hub::{
    HubError, OpenedPty, PtySource, RemoteSessionHub, SessionEvent, SessionOptions,
};
use parking_lot::Mutex;

#[derive(Default)]
struct State {
    output: VecDeque<u8>,
    input: Vec<u8>,
    exited: bool,
    waker: Option<Arc<Waker>>,
}

impl State {
    fn wake(&self) {
        if let Some(waker) = &self.waker {
            waker.wake().unwrap();
        }
    }
}

struct ControlledSource {
    state: Arc<Mutex<State>>,
    consumed: Sender<()>,
    reject: bool,
}

impl PtySource for ControlledSource {
    type Pty = ControlledPty;

    fn open(&self, options: &SessionOptions) -> io::Result<OpenedPty<ControlledPty>> {
        assert_eq!(options.shell, "controlled-shell");

        if self.reject {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "launch rejected",
            ));
        }

        Ok(OpenedPty {
            pty: ControlledPty {
                state: self.state.clone(),
                consumed: self.consumed.clone(),
                tokens: [Token(0); 3],
            },
            process_tree: None,
        })
    }
}

struct ControlledPty {
    state: Arc<Mutex<State>>,
    consumed: Sender<()>,
    tokens: [Token; 3],
}

impl Read for ControlledPty {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut state = self.state.lock();
        let count = buffer.len().min(state.output.len());

        if count == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }

        for slot in &mut buffer[..count] {
            *slot = state.output.pop_front().unwrap();
        }

        self.consumed.send(()).unwrap();

        Ok(count)
    }
}

impl Write for ControlledPty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.state.lock().input.extend_from_slice(bytes);

        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl ProcessReadWrite for ControlledPty {
    type Reader = Self;

    type Writer = Self;

    fn reader(&mut self) -> &mut Self::Reader {
        self
    }

    fn writer(&mut self) -> &mut Self::Writer {
        self
    }

    fn read_token(&self) -> Token {
        self.tokens[0]
    }

    fn write_token(&self) -> Token {
        self.tokens[1]
    }

    fn set_winsize(&mut self, _: WinsizeBuilder) -> io::Result<()> {
        Ok(())
    }

    fn register(
        &mut self,
        _: &Poll,
        tokens: &mut dyn Iterator<Item = Token>,
        _: Interest,
        waker: &Arc<Waker>,
    ) -> io::Result<()> {
        self.tokens = [
            tokens.next().unwrap(),
            tokens.next().unwrap(),
            tokens.next().unwrap(),
        ];

        self.state.lock().waker = Some(waker.clone());

        Ok(())
    }

    fn reregister(&mut self, _: &Poll, _: Interest) -> io::Result<()> {
        Ok(())
    }

    fn deregister(&mut self, _: &Poll) -> io::Result<()> {
        Ok(())
    }

    fn drain_ready(&self) -> Vec<Token> {
        let state = self.state.lock();
        let mut ready = vec![self.tokens[1]];

        if !state.output.is_empty() {
            ready.push(self.tokens[0]);
        }

        if state.exited {
            ready.push(self.tokens[2]);
        }

        ready
    }

    fn has_ready(&self) -> bool {
        let state = self.state.lock();

        !state.output.is_empty() || state.exited
    }
}

impl EventedPty for ControlledPty {
    fn child_event_token(&self) -> Token {
        self.tokens[2]
    }

    fn child_exited(&mut self) -> bool {
        self.state.lock().exited
    }
}

#[test]
fn supplied_output_survives_detach_and_exit_closes_subscriptions() {
    let state = Arc::new(Mutex::new(State::default()));
    let (consumed, reads) = channel();

    let hub = RemoteSessionHub::with_pty_source(ControlledSource {
        state: state.clone(),
        consumed,
        reject: false,
    });

    let id = hub
        .open(SessionOptions {
            shell: "controlled-shell".into(),
            ..SessionOptions::default()
        })
        .unwrap();

    let first = hub.attach(id).unwrap();

    drop(first);

    {
        let mut state = state.lock();

        state.output.extend(b"detached output");
        state.wake();
    }

    reads.recv_timeout(Duration::from_secs(2)).unwrap();

    let second = hub.attach(id).unwrap();

    assert!(
        second
            .snapshot()
            .vt
            .windows(b"detached output".len())
            .any(|part| part == b"detached output")
    );
    assert_eq!(hub.list_sessions()[0].attached_clients, 1);

    {
        let mut state = state.lock();

        state.exited = true;
        state.wake();
    }

    assert!(matches!(
        second
            .events()
            .recv_timeout(Duration::from_secs(2))
            .unwrap(),
        SessionEvent::Exited { .. }
    ));
    assert!(matches!(hub.attach(id), Err(HubError::SessionExited(exited)) if exited == id));

    hub.kill(id).unwrap();

    assert!(hub.list_sessions().is_empty());
}

#[test]
fn failed_creation_leaves_no_session_to_attach() {
    let (consumed, _) = channel();

    let hub = RemoteSessionHub::with_pty_source(ControlledSource {
        state: Arc::new(Mutex::new(State::default())),
        consumed,
        reject: true,
    });

    assert!(
        matches!(hub.open(SessionOptions { shell: "controlled-shell".into(), ..SessionOptions::default() }), Err(HubError::Spawn(error)) if error.kind() == io::ErrorKind::PermissionDenied)
    );
    assert!(hub.list_sessions().is_empty());
}
