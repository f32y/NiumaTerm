use crate::terminal::net_pty::*;

fn reader_with(bytes: &[u8]) -> (NetReader, SoftReady) {
    let ready = SoftReady::new();
    ready.set_ready();
    let reader = NetReader {
        buffer: Arc::new(Mutex::new(VecDeque::from(bytes.to_vec()))),
        read_ready: ready.clone(),
    };
    (reader, ready)
}

#[test]
fn reader_yields_bytes_then_signals_drained() {
    let (mut reader, ready) = reader_with(b"prompt> ");
    let mut buf = [0u8; 4];

    // First read gets a partial chunk; buffer still has data, stays ready.
    assert_eq!(reader.read(&mut buf).unwrap(), 4);
    assert_eq!(&buf, b"prom");
    assert!(ready.is_ready());

    // Second read drains the rest; ready clears so the loop can block.
    assert_eq!(reader.read(&mut buf).unwrap(), 4);
    assert_eq!(&buf, b"pt> ");
    assert!(!ready.is_ready());

    // Empty read reports caught-up (Ok(0)) and keeps ready clear.
    assert_eq!(reader.read(&mut buf).unwrap(), 0);
    assert!(!ready.is_ready());
}

#[test]
fn buffer_overflow_keeps_the_newest_bytes() {
    let mut queue = VecDeque::from(vec![b'x'; MAX_BUFFERED_BYTES]);
    assert!(!push_bounded(&mut queue, Vec::new()), "at the cap is fine");

    assert!(push_bounded(&mut queue, b"tail".to_vec()));
    assert_eq!(queue.len(), MAX_BUFFERED_BYTES);
    let kept: Vec<u8> = queue.iter().rev().take(4).rev().copied().collect();
    assert_eq!(kept, b"tail");
}

#[test]
fn late_bytes_reset_readiness() {
    let (mut reader, ready) = reader_with(b"x");
    let mut buf = [0u8; 8];
    assert_eq!(reader.read(&mut buf).unwrap(), 1);
    assert!(!ready.is_ready());

    // Simulate the drain thread appending live output.
    reader.buffer.lock().extend(b"y".iter().copied());
    ready.set_ready();
    assert_eq!(reader.read(&mut buf).unwrap(), 1);
    assert_eq!(buf[0], b'y');
    assert!(!ready.is_ready());
}
