use std::collections;
use std::sync::Arc;

use crate::frame::TerminalFrame;
use crate::graphics;

#[derive(Default)]
pub(crate) struct TerminalFrameCache {
    frame: Option<TerminalFrame>,
    /// The frame no longer matches the surface and must be rebuilt on the next
    /// render. `frame` is kept: pointer/IME mapping between the invalidation
    /// and the rebuild must keep using what is on screen — mapping against an
    /// empty cache flips the row offsets mid-drag (broken-selection bug).
    stale: bool,
    full_invalidation: bool,
}

pub(super) type GenerationMap = collections::HashMap<u32, Arc<graphics::ImageGeneration>>;

impl TerminalFrameCache {
    /// The last built frame — served even when stale, so consumers between an
    /// invalidation and the next render keep mapping against what is displayed.
    pub(crate) fn current(&self) -> Option<TerminalFrame> {
        self.frame.clone()
    }

    pub(crate) fn needs_rebuild(&self) -> bool {
        self.stale || self.frame.is_none()
    }

    pub(crate) fn rebuild(&mut self, frame: TerminalFrame) {
        self.frame = Some(frame);
        self.stale = false;
        self.full_invalidation = false;
    }

    pub(crate) fn invalidate(&mut self) {
        self.stale = true;
    }

    pub(crate) fn invalidate_full(&mut self) {
        self.stale = true;
        self.full_invalidation = true;
    }

    pub(crate) fn reusable_frame(&self) -> Option<TerminalFrame> {
        (!self.full_invalidation)
            .then(|| self.frame.clone())
            .flatten()
    }
}
