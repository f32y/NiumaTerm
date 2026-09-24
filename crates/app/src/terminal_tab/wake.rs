#[cfg(test)]
#[path = "wake_tests.rs"]
mod wake_tests;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use nmt_terminal::session::SessionChange;

pub(super) type WakeReceiver = UnboundedReceiver<SessionChange>;

/// Wakes a pane from the PTY thread without an async runtime there. Content
/// changes coalesce until the pane renders; host-event changes bypass that
/// pending bit because a background pane may not render but its tab and
/// workspace indicators must still update.
#[derive(Clone)]
pub(super) struct WakeSignal {
    queued: Arc<AtomicBool>,
    resignal: Arc<AtomicBool>,
    tx: UnboundedSender<SessionChange>,
}

pub(super) fn wake_channel() -> (WakeSignal, WakeReceiver) {
    let (tx, rx) = unbounded();

    (
        WakeSignal {
            queued: Arc::new(AtomicBool::new(false)),
            resignal: Arc::new(AtomicBool::new(false)),
            tx,
        },
        rx,
    )
}

impl WakeSignal {
    pub(super) fn signal(&self, change: SessionChange) -> bool {
        if change == SessionChange::HostEvents {
            let _ = self.tx.unbounded_send(change);

            return true;
        }

        if self.queued.swap(true, Ordering::AcqRel) {
            self.resignal.store(true, Ordering::Release);

            return false;
        }

        let _ = self.tx.unbounded_send(change);

        true
    }

    pub(super) fn mark_delivered(&self) {
        self.queued.store(false, Ordering::Release);

        if self.resignal.swap(false, Ordering::AcqRel) {
            self.signal(SessionChange::Content);
        }
    }
}
