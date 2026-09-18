use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{self, ErrorKind};
use std::task::{Context, Poll};

use nmt_platform::AsyncPty;

/// Bound one write pass so a writer that keeps accepting cannot monopolize
/// the owner task ahead of output reads and queued commands.
const MAX_WRITE_BATCH: usize = 64 * 1024;

#[derive(Default)]
pub struct PtyState {
    pub(super) write_list: VecDeque<Cow<'static, [u8]>>,
    writing: Option<Writing>,
}

impl PtyState {
    /// Write queued input to `writer` until it is drained or the writer would
    /// block. A partially written chunk stays current, so the next writable
    /// event resumes it where this one stopped.
    #[inline]
    pub(super) fn write_to(
        &mut self,
        writer: &mut impl AsyncPty,
        cx: &mut Context<'_>,
    ) -> io::Result<usize> {
        let mut written = 0;

        self.ensure_next();

        'write_many: while let Some(mut current) = self.take_current() {
            'write_one: loop {
                if current.finished() {
                    self.goto_next();

                    break 'write_one;
                }

                if written >= MAX_WRITE_BATCH {
                    self.set_current(Some(current));

                    break 'write_many;
                }

                let remaining = current.remaining_bytes();

                let result = match writer.poll_write(
                    cx,
                    &remaining[..remaining.len().min(MAX_WRITE_BATCH - written)],
                ) {
                    Poll::Ready(result) => result,
                    Poll::Pending => {
                        self.set_current(Some(current));

                        break 'write_many;
                    }
                };

                match result {
                    Ok(0) => {
                        self.set_current(Some(current));

                        return Err(ErrorKind::WriteZero.into());
                    }
                    Ok(n) => {
                        current.advance(n);

                        written += n;

                        if current.finished() {
                            self.goto_next();

                            break 'write_one;
                        }
                    }
                    Err(err) => {
                        self.set_current(Some(current));

                        match err.kind() {
                            ErrorKind::Interrupted | ErrorKind::WouldBlock => break 'write_many,
                            _ => return Err(err),
                        }
                    }
                }
            }
        }

        Ok(written)
    }

    #[inline]
    fn ensure_next(&mut self) {
        if self.writing.is_none() {
            self.goto_next();
        }
    }

    #[inline]
    fn goto_next(&mut self) {
        self.writing = self.write_list.pop_front().map(Into::into);
    }

    #[inline]
    fn take_current(&mut self) -> Option<Writing> {
        self.writing.take()
    }

    #[inline]
    pub(super) fn needs_write(&self) -> bool {
        self.writing.is_some() || !self.write_list.is_empty()
    }

    #[inline]
    fn set_current(&mut self, new: Option<Writing>) {
        self.writing = new;
    }
}

struct Writing {
    source: Cow<'static, [u8]>,
    written: usize,
}

impl Writing {
    #[inline]
    fn advance(&mut self, n: usize) {
        self.written += n;
    }

    #[inline]
    fn remaining_bytes(&self) -> &[u8] {
        &self.source[self.written..]
    }

    #[inline]
    fn finished(&self) -> bool {
        self.written >= self.source.len()
    }
}

impl From<Cow<'static, [u8]>> for Writing {
    #[inline]
    fn from(c: Cow<'static, [u8]>) -> Self {
        Writing {
            source: c,
            written: 0,
        }
    }
}
