use std::io::Read;

pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

pub(crate) fn read_message(reader: impl Read) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();

    reader
        .take((MAX_MESSAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;

    (bytes.len() <= MAX_MESSAGE_BYTES).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor, Read};

    use crate::ipc_message::{MAX_MESSAGE_BYTES, read_message};

    #[test]
    fn accepts_complete_messages_and_rejects_excess_or_failed_reads() {
        let bytes = vec![b'x'; MAX_MESSAGE_BYTES];

        assert_eq!(read_message(bytes.as_slice()), Some(bytes));
        assert!(read_message(vec![0; MAX_MESSAGE_BYTES + 1].as_slice()).is_none());

        let mut endless = io::repeat(0);

        assert!(read_message(&mut endless).is_none());

        struct FailedRead;

        impl Read for FailedRead {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }

        assert!(read_message(Cursor::new(b"partial").chain(FailedRead)).is_none());
    }
}
