#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod diagnostics_tests;

use std::collections::VecDeque;
use std::io::{Result, Write};
use std::sync::{Arc, LazyLock, Once};

use parking_lot::Mutex;

const MAX_LOG_BYTES: usize = 256 * 1024;
const MAX_EVENT_BYTES: usize = 16 * 1024;

static LOG: LazyLock<Arc<Mutex<LogBuffer>>> = LazyLock::new(Arc::default);
static INITIALIZE: Once = Once::new();

#[derive(Default)]
struct LogBuffer {
    entries: VecDeque<String>,
    bytes: usize,
}

impl LogBuffer {
    fn push(&mut self, entry: String) {
        self.bytes += entry.len();

        self.entries.push_back(entry);

        while self.bytes > MAX_LOG_BYTES {
            if let Some(oldest) = self.entries.pop_front() {
                self.bytes -= oldest.len();
            }
        }
    }

    fn snapshot(&self) -> String {
        self.entries.iter().map(String::as_str).collect()
    }
}

/// Format each event before taking the shared lock, so concurrent events
/// remain separate and reading logs never waits on message formatting.
struct LogWriter {
    log: Arc<Mutex<LogBuffer>>,
    bytes: Vec<u8>,
}

impl Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> Result<usize> {
        let available = MAX_EVENT_BYTES.saturating_sub(self.bytes.len());

        self.bytes
            .extend_from_slice(&bytes[..bytes.len().min(available)]);

        Ok(bytes.len())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        if self.bytes.is_empty() {
            return;
        }

        let mut entry = String::from_utf8_lossy(&self.bytes).into_owned();

        if !entry.ends_with('\n') {
            entry.push('\n');
        }

        self.log.lock().push(entry);
    }
}

pub(crate) fn initialize() {
    INITIALIZE.call_once(|| {
        let result = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_env_filter("warn,nmt_mobile=debug,nmt_remote=info")
            .with_writer(|| LogWriter {
                log: Arc::clone(&LOG),
                bytes: Vec::new(),
            })
            .try_init();

        if let Err(error) = result {
            LOG.lock()
                .push(format!("Could not start diagnostic logging: {error}\n"));
        }
    });
}

/// Diagnostics from this app launch, with the oldest events removed once
/// the buffer reaches its size limit.
#[uniffi::export]
pub fn diagnostic_log() -> String {
    LOG.lock().snapshot()
}

#[uniffi::export]
pub fn clear_diagnostic_log() {
    *LOG.lock() = LogBuffer::default();
}
