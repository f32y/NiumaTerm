//! Native key-event state (copied from winit so this crate no longer depends on
//! winit). Only the pieces the terminal encoder uses.

/// Whether a key is being pressed or released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementState {
    Pressed,
    Released,
}

impl ElementState {
    pub fn is_pressed(self) -> bool {
        matches!(self, ElementState::Pressed)
    }
}
