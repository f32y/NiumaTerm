use crate::clipboard::ClipboardType;
use crate::event::BlockEvent;
use crate::graphics::UpdateQueues;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionChange {
    Content,
    HostEvents,
}

/// Receives synchronous updates on the PTY worker. Implementations must not
/// access windows, wait for another thread, or call back into the session:
/// terminal events can originate while the engine is locked.
pub trait SessionObserver: Send + Sync {
    fn graphics(&self, _updates: UpdateQueues) {}
    fn blocks(&self, _events: &[BlockEvent]) {}
    fn clipboard(&self, _kind: ClipboardType, _text: String) {}
    fn changed(&self, _change: SessionChange) {}
}
