use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use nmt_config::CursorShape;
use nmt_config::colors::Colors;
use nmt_config::system::NewlineShortcut;
use nmt_platform::{
    ChildEvent, EventedPty, Interest, Poll, ProcessReadWrite, Token, Waker, WinsizeBuilder,
};
use nmt_terminal::pty_pipe::SessionOptions;
use nmt_terminal::session::TerminalSession;
use parking_lot::Mutex;

use crate::block_list::chrome::DurationLabels;
use crate::frame_source::TerminalFrameSource;
use crate::metrics::CellMetrics;
use crate::pane_model::{FrameTheme, PaneController, PaneSettings};
use crate::wake::wake_channel;

struct TestPty {
    output: VecDeque<u8>,
    input: Arc<Mutex<Vec<u8>>>,
    read_token: Token,
    write_token: Token,
    child_token: Token,
}

impl Read for TestPty {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = buffer.len().min(self.output.len());
        for slot in &mut buffer[..count] {
            *slot = self.output.pop_front().unwrap();
        }
        Ok(count)
    }
}

impl Write for TestPty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.input.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl ProcessReadWrite for TestPty {
    type Reader = Self;
    type Writer = Self;
    fn reader(&mut self) -> &mut Self {
        self
    }
    fn writer(&mut self) -> &mut Self {
        self
    }
    fn read_token(&self) -> Token {
        self.read_token
    }
    fn write_token(&self) -> Token {
        self.write_token
    }
    fn set_winsize(&mut self, _: WinsizeBuilder) -> io::Result<()> {
        Ok(())
    }
    fn register(
        &mut self,
        _: &Poll,
        tokens: &mut dyn Iterator<Item = Token>,
        _: Interest,
        _: &Arc<Waker>,
    ) -> io::Result<()> {
        self.read_token = tokens.next().unwrap();
        self.write_token = tokens.next().unwrap();
        self.child_token = tokens.next().unwrap();
        Ok(())
    }
    fn reregister(&mut self, _: &Poll, _: Interest) -> io::Result<()> {
        Ok(())
    }
    fn deregister(&mut self, _: &Poll) -> io::Result<()> {
        Ok(())
    }
    fn drain_ready(&self) -> Vec<Token> {
        let mut tokens = vec![self.write_token];
        if !self.output.is_empty() {
            tokens.push(self.read_token);
        }
        tokens
    }
    fn has_ready(&self) -> bool {
        !self.output.is_empty()
    }
}

impl EventedPty for TestPty {
    fn child_event_token(&self) -> Token {
        self.child_token
    }
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        None
    }
}

pub(crate) fn controller(vt: &[u8], engine_blocks: bool) -> (PaneController, Arc<Mutex<Vec<u8>>>) {
    let input = Arc::new(Mutex::new(Vec::new()));
    let mut output = vt.to_vec();
    output.extend_from_slice(b"\x1b]0;controller-ready\x07");
    let pty = TestPty {
        output: output.into(),
        input: input.clone(),
        read_token: Token(0),
        write_token: Token(0),
        child_token: Token(0),
    };
    let source = TerminalFrameSource::attach(wake_channel().0, 1, |observer| {
        TerminalSession::from_pty(
            pty,
            None,
            SessionOptions {
                cols: 40,
                rows: 6,
                route_id: 1,
                colors: Colors::default(),
                cursor_shape: CursorShape::Block,
                scrollback_lines: 100,
                engine_blocks,
                terminal_responses: true,
                output_sink: None,
            },
            Some(observer),
        )
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while source.session.title() != "controller-ready" {
        assert!(
            Instant::now() < deadline,
            "the test output must reach the engine"
        );
        thread::sleep(Duration::from_millis(1));
    }
    let settings = PaneSettings {
        fixed_bottom: false,
        pad_rows: 1.0,
        show_block_chrome: true,
        smooth_wheel: true,
        scroll_to_bottom_when_typing: true,
        newline_shortcut: NewlineShortcut::ShiftEnter,
        cursor_shape: CursorShape::Block,
    };
    let mut controller = PaneController::new(
        source,
        settings,
        FrameTheme::default(),
        DurationLabels::default(),
    );
    controller.cell_metrics = Some(CellMetrics {
        width_px: 8.0,
        height_px: 18.0,
    });
    controller.content_size = (320.0, 108.0);
    controller.refresh_frame();
    (controller, input)
}

pub(crate) fn assert_input(input: &Mutex<Vec<u8>>, expected: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while input.lock().len() < expected.len() {
        assert!(
            Instant::now() < deadline,
            "terminal input did not reach the PTY"
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(*input.lock(), expected);
}
