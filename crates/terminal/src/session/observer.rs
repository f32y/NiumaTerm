use crate::clipboard::ClipboardType;
use crate::event::BlockEvent;
use crate::graphics::UpdateQueues;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionChange {
    Content,
    HostEvents,
}

/// Receives synchronous updates on the PTY worker. Implementations must not
/// access windows or wait for another thread: a callback can run during a
/// parse batch, and waiting for an enqueued command would stall its owner.
pub trait SessionObserver: Send + Sync {
    fn graphics(&self, _updates: UpdateQueues) {}
    fn blocks(&self, _events: &[BlockEvent]) {}
    fn clipboard(&self, _kind: ClipboardType, _text: String) {}
    fn changed(&self, _change: SessionChange) {}
}
