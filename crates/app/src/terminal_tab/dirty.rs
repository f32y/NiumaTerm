//! Demand-driven render state: the shell coalesces redraw
//! requests into one pending bit, drawing once per `RedrawRequested`.

#[cfg(test)]
#[path = "dirty_tests.rs"]
mod dirty_tests;

#[derive(Default)]
pub(super) struct DirtyState {
    pending: bool,
}

impl DirtyState {
    pub(super) fn mark(&mut self) -> bool {
        let was_clean = !self.pending;

        self.pending = true;

        was_clean
    }

    pub(super) fn begin_frame(&mut self) -> bool {
        if !self.pending {
            return false;
        }

        self.pending = false;

        true
    }

    #[cfg(test)]
    fn is_pending(&self) -> bool {
        self.pending
    }
}
