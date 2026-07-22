// Single-producer single-consumer buffer for Rust

use std::cell::UnsafeCell;
use std::io::{self, Read, Write};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

// Per-byte UnsafeCell instead of UnsafeCell<Box<[u8]>>: reader and writer copy
// through raw pointers derived from a shared `&[UnsafeCell<u8>]`, so neither
// side ever materializes a `&mut` over the whole buffer while the other thread
// accesses its disjoint region. UnsafeCell<u8> is repr(transparent), so the
// slice base pointer can be cast to *mut u8 for bulk copies.
struct SpscBuffer {
    buf: Box<[UnsafeCell<u8>]>,
    len: AtomicUsize,
}

// Safety: all cross-thread data access goes through the reader/writer halves,
// whose disjoint [start, len) / [end, capacity-len) regions are synchronized
// by the atomic `len` (release on publish, acquire on consume via SeqCst).
// The UnsafeCell contents are never touched through `&SpscBuffer` directly.
unsafe impl Send for SpscBuffer {}
unsafe impl Sync for SpscBuffer {}

impl SpscBuffer {
    fn new(size: usize) -> Self {
        Self {
            buf: (0..size).map(|_| UnsafeCell::new(0)).collect(),
            len: AtomicUsize::new(0),
        }
    }

    fn len(&self) -> usize {
        self.len.load(Ordering::SeqCst)
    }

    fn capacity(&self) -> usize {
        self.buf.len()
    }

    fn data_ptr(&self) -> *mut u8 {
        self.buf.as_ptr() as *mut u8
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn is_full(&self) -> bool {
        self.len() == self.capacity()
    }
}

/// Consumer of the ringbuffer.
pub struct SpscBufferReader {
    start: usize,
    buffer: Arc<SpscBuffer>,
}

impl SpscBufferReader {
    /// Get length of contents currently in the buffer
    #[allow(unused)]
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Get total capacity of the buffer
    #[allow(unused)]
    pub fn capacity(&self) -> usize {
        self.buffer.capacity()
    }

    /// Check whether the buffer is currently empty
    #[allow(unused)]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Check whether the buffer is currently empty
    #[allow(unused)]
    pub fn is_full(&self) -> bool {
        self.buffer.is_full()
    }

    /// Read data from the buffer. Returns number of bytes read.
    pub fn read_to_slice(&mut self, buf: &mut [u8]) -> usize {
        use std::cmp::min;

        let ringbuf_capacity = self.buffer.capacity();
        let ringbuf_len = self.buffer.len.load(Ordering::SeqCst);

        // Max number of bytes we might read
        let max_read_size = min(buf.len(), ringbuf_len);
        let contents_until_end = ringbuf_capacity - self.start;
        let read_size = min(max_read_size, contents_until_end);

        // Safety: `len` guarantees [start, start + read_size) holds published
        // data the writer will not touch until we fetch_sub below, and the
        // range is in bounds of the allocation.
        unsafe {
            ptr::copy_nonoverlapping(
                self.buffer.data_ptr().add(self.start),
                buf.as_mut_ptr(),
                read_size,
            );
        }
        self.start = (self.start + read_size) % ringbuf_capacity;
        self.buffer.len.fetch_sub(read_size, Ordering::SeqCst);

        read_size
    }
}

impl Read for SpscBufferReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Ok(self.read_to_slice(buf))
    }
}

/// Producer for the ringbuffer
pub struct SpscBufferWriter {
    end: usize,
    buffer: Arc<SpscBuffer>,
}

impl SpscBufferWriter {
    /// Get length of contents currently in the buffer
    #[allow(unused)]
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Get total capacity of the buffer
    #[allow(unused)]
    pub fn capacity(&self) -> usize {
        self.buffer.capacity()
    }

    /// Check whether the buffer is currently empty
    #[allow(unused)]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Check whether the buffer is currently empty
    pub fn is_full(&self) -> bool {
        self.buffer.is_full()
    }

    /// Write data to the buffer. Returns number of bytes written.
    pub fn write_from_slice(&mut self, buf: &[u8]) -> usize {
        use std::cmp::min;

        let ringbuf_capacity = self.buffer.capacity();
        let ringbuf_len = self.buffer.len.load(Ordering::SeqCst);

        // Max number of bytes we might write
        let max_write_size = min(buf.len(), ringbuf_capacity - ringbuf_len);
        let space_until_end = ringbuf_capacity - self.end;
        let write_size = min(max_write_size, space_until_end);

        // Safety: `len` guarantees [end, end + write_size) is free space the
        // reader will not touch until we fetch_add below, and the range is in
        // bounds of the allocation.
        unsafe {
            ptr::copy_nonoverlapping(
                buf.as_ptr(),
                self.buffer.data_ptr().add(self.end),
                write_size,
            );
        }
        self.end = (self.end + write_size) % ringbuf_capacity;
        self.buffer.len.fetch_add(write_size, Ordering::SeqCst);

        write_size
    }
}

impl Write for SpscBufferWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(self.write_from_slice(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Create a new SPSC buffer pair.
///
/// The producer and consumer can safely be transferred between threads; the
/// expected use case is that one thread will be writing and one will be reading.
///
/// The underlying buffer's size is synchronised using an atomic. The producer
/// and consumer have methods to query the size and the capacity, which is
/// guaranteed to be consistent between threads but may not be sufficient to
/// prevent races depending on what you are trying to achieve.
///
/// See the mio-anonymous-pipes crate for example usage.
pub fn spsc_buffer(size: usize) -> (SpscBufferWriter, SpscBufferReader) {
    // Arc, not Rc: reader and writer live on different threads, so the final
    // two drops can race; a non-atomic refcount would be a data race (UB).
    let buffer = Arc::new(SpscBuffer::new(size));

    let producer = SpscBufferWriter {
        end: 0,
        buffer: buffer.clone(),
    };
    let consumer = SpscBufferReader { start: 0, buffer };

    (producer, consumer)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_spsc_buffer() {
        let buf = [1u8; 100];

        let (mut producer, mut consumer) = spsc_buffer(60);

        assert!(producer.is_empty());
        assert!(consumer.is_empty());

        assert_eq!(producer.len(), 0);
        assert_eq!(consumer.len(), 0);

        assert_eq!(producer.capacity(), 60);
        assert_eq!(consumer.capacity(), 60);

        let mut out_buf = [0u8; 100];

        assert_eq!(producer.write_from_slice(&buf), 60);
        assert_eq!(producer.len(), 60);
        assert_eq!(consumer.len(), 60);

        assert_eq!(consumer.read_to_slice(&mut out_buf), 60);
        assert_eq!(producer.len(), 0);
        assert_eq!(consumer.len(), 0);

        assert_eq!(producer.write_from_slice(&buf[60..]), 40);
        assert_eq!(producer.len(), 40);
        assert_eq!(consumer.len(), 40);

        assert_eq!(consumer.read_to_slice(&mut out_buf[60..]), 40);
        assert_eq!(producer.len(), 0);
        assert_eq!(consumer.len(), 0);

        assert_eq!(&buf[..], &out_buf[..]);
    }
}
