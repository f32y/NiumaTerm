use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::task::{Context, Poll as TaskPoll};
use std::thread;
use std::time::{Duration, Instant};

use futures::task::AtomicWaker;
use gpui::{FontFallbacks, px};
use nmt_config::CursorShape;
use nmt_config::appearance::InputStyle;
use nmt_config::colors::Colors;
use nmt_config::system::NewlineShortcut;
use nmt_platform::{AsyncPty, WinsizeBuilder, poll_nonblocking};
use nmt_terminal::clipboard::ClipboardType;
use nmt_terminal::session::TerminalSession;
use nmt_terminal::termio::SessionOptions;
use parking_lot::Mutex;

use crate::terminal_tab::block_list::chrome::DurationLabels;
use crate::terminal_tab::frame_source::TerminalFrameSource;
use crate::terminal_tab::metrics::CellMetrics;
use crate::terminal_tab::pane_model::{ClipboardAccess, FrameTheme, PaneController};
use crate::terminal_tab::settings::TerminalSettings;
use crate::terminal_tab::wake::wake_channel;

struct TestPty {
    output: Arc<TestOutput>,
    input: Arc<Mutex<Vec<u8>>>,
}

#[derive(Default)]
pub(crate) struct TestOutput {
    bytes: Mutex<VecDeque<u8>>,
    task_waker: AtomicWaker,
}

impl TestOutput {
    pub(crate) fn push(&self, bytes: &[u8]) {
        self.bytes.lock().extend(bytes);

        self.task_waker.wake();
    }
}

#[derive(Clone, Default)]
pub(crate) struct TestClipboard {
    pub text: Arc<Mutex<Option<String>>>,
    pub reject_writes: bool,
}

impl ClipboardAccess for TestClipboard {
    fn read(&mut self) -> Option<String> {
        self.text.lock().clone()
    }

    fn write(&mut self, _kind: ClipboardType, text: String) -> bool {
        if self.reject_writes {
            return false;
        }

        *self.text.lock() = Some(text);

        true
    }
}

impl Read for TestPty {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut output = self.output.bytes.lock();

        let count = buffer.len().min(output.len());

        for slot in &mut buffer[..count] {
            *slot = output.pop_front().unwrap();
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

impl AsyncPty for TestPty {
    fn poll_read(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> TaskPoll<io::Result<usize>> {
        self.output.task_waker.register(cx.waker());

        match self.read(buf) {
            Ok(0) => TaskPoll::Pending,
            result => poll_nonblocking(cx, result),
        }
    }

    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> TaskPoll<io::Result<usize>> {
        poll_nonblocking(cx, self.write(buf))
    }

    fn poll_exit(&mut self, _: &mut Context<'_>) -> TaskPoll<()> {
        TaskPoll::Pending
    }

    fn poll_resize(&mut self, _: &mut Context<'_>, _: WinsizeBuilder) -> TaskPoll<io::Result<()>> {
        TaskPoll::Ready(Ok(()))
    }
}

pub(crate) fn controller(vt: &[u8], engine_blocks: bool) -> (PaneController, Arc<Mutex<Vec<u8>>>) {
    let (controller, input, _) = streaming_controller(vt, engine_blocks);

    (controller, input)
}

pub(crate) fn streaming_controller(
    vt: &[u8],
    engine_blocks: bool,
) -> (PaneController, Arc<Mutex<Vec<u8>>>, Arc<TestOutput>) {
    let input = Arc::new(Mutex::new(Vec::new()));
    let output = Arc::new(TestOutput::default());

    output.push(vt);
    output.push(b"\x1b]0;controller-ready\x07");

    let pty = TestPty {
        output: output.clone(),
        input: input.clone(),
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

    let settings = TerminalSettings {
        input_style: InputStyle::Waterfall,
        manage_subprocess_job: false,
        command_blocks: true,
        font_family: "Consolas".into(),
        font_size: 14.0,
        line_height: 1.0,
        background_opacity: 1.0,
        corner_radius: px(0.0),
        font_fallbacks: FontFallbacks::default(),
        smooth_wheel: true,
        scroll_to_bottom_when_typing: true,
        newline_shortcut: NewlineShortcut::ShiftEnter,
        cursor_shape: CursorShape::Block,
        improve_powershell_compatibility: true,
    };

    let mut controller = PaneController::new(
        source,
        settings,
        FrameTheme::default(),
        DurationLabels::default(),
        Box::<TestClipboard>::default(),
    );

    controller.cell_metrics = Some(CellMetrics {
        width_px: 8.0,
        height_px: 18.0,
    });

    controller.content_size = (320.0, 108.0);

    controller.refresh_frame();

    (controller, input, output)
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
