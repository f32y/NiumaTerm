use tokio::io::{AsyncRead, AsyncReadExt as _};

pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

pub(crate) async fn read_message(reader: impl AsyncRead + Unpin) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();

    reader
        .take((MAX_MESSAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await
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
            nmt_runtime::handle().block_on(read_message(bytes.as_slice())),
            Some(bytes)
        );
        assert!(
            nmt_runtime::handle()
                .block_on(read_message(vec![0; MAX_MESSAGE_BYTES + 1].as_slice()))
                .is_none()
        );
        assert!(
            nmt_runtime::handle()
                .block_on(read_message(repeat(0)))
                .is_none()
        );

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
            nmt_runtime::handle()
                .block_on(read_message(Cursor::new(b"partial").chain(FailedRead)))
                .is_none()
        );
    }
}
