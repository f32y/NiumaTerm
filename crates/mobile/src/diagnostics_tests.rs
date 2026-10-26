use std::io::Write as _;
use std::sync::Arc;

use parking_lot::Mutex;
use tracing::subscriber::with_default;

use crate::diagnostics::{LogBuffer, LogWriter, MAX_EVENT_BYTES, MAX_LOG_BYTES};

#[test]
fn diagnostic_logging_records_formatted_events_and_filters_debug_output() {
    let log = Arc::new(Mutex::new(LogBuffer::default()));
    let writer_log = Arc::clone(&log);

    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || LogWriter {
            log: Arc::clone(&writer_log),
            bytes: Vec::new(),
        })
        .finish();

    with_default(subscriber, || {
        tracing::info!(host = "test-host", "connection ready");
        tracing::debug!("hidden detail");
        tracing::warn!("retry failed");
    });

    let snapshot = log.lock().snapshot();

    assert!(snapshot.contains("connection ready"));
    assert!(snapshot.contains("test-host"));
    assert!(snapshot.contains("retry failed"));
    assert!(!snapshot.contains("hidden detail"));
    assert!(!snapshot.contains('\u{1b}'));
}

#[test]
fn diagnostic_buffer_removes_old_events_and_limits_large_events() {
    let log = Arc::new(Mutex::new(LogBuffer::default()));

    for _ in 0..(MAX_LOG_BYTES / MAX_EVENT_BYTES + 2) {
        let mut writer = LogWriter {
            log: Arc::clone(&log),
            bytes: Vec::new(),
        };

        writer.write_all(&vec![b'x'; MAX_EVENT_BYTES * 2]).unwrap();

        assert_eq!(writer.bytes.len(), MAX_EVENT_BYTES);
    }

    let mut log = log.lock();

    log.push("latest event\n".into());

    assert!(log.bytes <= MAX_LOG_BYTES);
    assert!(log.snapshot().ends_with("latest event\n"));

    *log = LogBuffer::default();

    assert!(log.snapshot().is_empty());
    assert_eq!(log.bytes, 0);
}

#[test]
fn split_utf8_writes_are_combined_before_display() {
    let log = Arc::new(Mutex::new(LogBuffer::default()));

    let mut writer = LogWriter {
        log: Arc::clone(&log),
        bytes: Vec::new(),
    };

    let text = "connection \u{2713}\n";
    let bytes = text.as_bytes();

    writer.write_all(&bytes[..12]).unwrap();
    writer.write_all(&bytes[12..]).unwrap();

    drop(writer);

    assert_eq!(log.lock().snapshot(), text);
}
