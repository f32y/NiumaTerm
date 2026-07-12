//! Demand-driven render state: the shell coalesces redraw
//! requests into one pending bit, drawing once per `RedrawRequested`.

#[derive(Default)]
pub(crate) struct DirtyState {
    pending: bool,
}

impl DirtyState {
    pub(crate) fn mark(&mut self) -> bool {
        let was_clean = !self.pending;
        self.pending = true;
        was_clean
    }

    pub(crate) fn begin_frame(&mut self) -> bool {
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

#[cfg(test)]
mod tests {
    use crate::terminal::dirty::DirtyState;

    #[test]
    fn dirty_state_coalesces_until_frame_begins() {
        let mut dirty = DirtyState::default();

        assert!(dirty.mark());
        assert!(!dirty.mark());
        assert!(dirty.is_pending());

        assert!(dirty.begin_frame());
        assert!(!dirty.is_pending());
        assert!(!dirty.begin_frame());
    }
}
