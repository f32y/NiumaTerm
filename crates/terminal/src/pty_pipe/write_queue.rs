use std::borrow::Cow;
use std::collections::VecDeque;

#[derive(Default)]
pub struct PtyState {
    pub(super) write_list: VecDeque<Cow<'static, [u8]>>,
    writing: Option<Writing>,
}

impl PtyState {
    #[inline]
    pub(super) fn ensure_next(&mut self) {
        if self.writing.is_none() {
            self.goto_next();
        }
    }

    #[inline]
    pub(super) fn goto_next(&mut self) {
        self.writing = self.write_list.pop_front().map(Writing::new);
    }

    #[inline]
    pub(super) fn take_current(&mut self) -> Option<Writing> {
        self.writing.take()
    }

    #[inline]
    pub(super) fn needs_write(&self) -> bool {
        self.writing.is_some() || !self.write_list.is_empty()
    }

    #[inline]
    pub(super) fn set_current(&mut self, new: Option<Writing>) {
        self.writing = new;
    }
}

pub(super) struct Writing {
    source: Cow<'static, [u8]>,
    written: usize,
}

impl Writing {
    #[inline]
    fn new(c: Cow<'static, [u8]>) -> Writing {
        Writing {
            source: c,
            written: 0,
        }
    }

    #[inline]
    pub(super) fn advance(&mut self, n: usize) {
        self.written += n;
    }

    #[inline]
    pub(super) fn remaining_bytes(&self) -> &[u8] {
        &self.source[self.written..]
    }

    #[inline]
    pub(super) fn finished(&self) -> bool {
        self.written >= self.source.len()
    }
}
