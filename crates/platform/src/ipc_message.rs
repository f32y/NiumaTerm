use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::time::timeout;

pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// How long one client may take to send its message and close. Servers read
/// one client at a time, and a sender writes a single line and disconnects,
/// so a client that stays connected past this is holding every later one
/// back.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) async fn read_message(reader: impl AsyncRead + Unpin) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut limited = reader.take((MAX_MESSAGE_BYTES + 1) as u64);

    timeout(READ_TIMEOUT, limited.read_to_end(&mut bytes))
        .await
        .ok()?
        .ok()?;

    (bytes.len() <= MAX_MESSAGE_BYTES).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor};
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use tokio::io::{AsyncRead, AsyncReadExt as _, ReadBuf, repeat};

    use crate::ipc_message::{MAX_MESSAGE_BYTES, read_message};

    #[test]
    fn accepts_complete_messages_and_rejects_excess_or_failed_reads() {
        let bytes = vec![b'x'; MAX_MESSAGE_BYTES];

        assert_eq!(
            crate::runtime().block_on(read_message(bytes.as_slice())),
            Some(bytes)
        );
        assert!(
            crate::runtime()
                .block_on(read_message(vec![0; MAX_MESSAGE_BYTES + 1].as_slice()))
                .is_none()
        );
        assert!(crate::runtime().block_on(read_message(repeat(0))).is_none());

        struct FailedRead;

        impl AsyncRead for FailedRead {
            fn poll_read(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
                _: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
            }
        }

        assert!(
            crate::runtime()
                .block_on(read_message(Cursor::new(b"partial").chain(FailedRead)))
                .is_none()
        );
    }
}
