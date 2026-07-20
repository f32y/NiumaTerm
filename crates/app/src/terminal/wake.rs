use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

/// A render wakeup signaled from the blocking PTY thread. The PTY thread
/// sends these synchronously via [`WakeSender`]; the GPUI pane wake path forwards
/// them into GPUI notifications. No async runtime is added: the PTY stays a
/// blocking thread without sharing the UI executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// Terminal content changed (PTY damage) or a UI repaint was requested. Carries
    /// the source surface id so the shell repaints only when it is the active tab.
    Content(u64),
    /// A user-visible `HostEvent` was enqueued on the surface with this id. Renders
    /// and rebuilds chrome.
    Chrome(u64),
}

/// Thread-safe wakeup handle cloned into every session's listener. Posts `Wake`s
/// through the supplied closure, so the blocking PTY thread wakes the GPUI pane
/// with no intermediate channel or bridge thread. Callable from any thread.
#[derive(Clone)]
pub struct WakeSender(Arc<dyn Fn(Wake) + Send + Sync>);

impl WakeSender {
    pub fn from_fn(send: impl Fn(Wake) + Send + Sync + 'static) -> Self {
        Self(Arc::new(send))
    }

    /// Post a wakeup into the event loop. A closed loop (window torn down) is ignored.
    pub fn send(&self, wake: Wake) {
        (self.0)(wake);
    }
}

pub(crate) type WakeReceiver = UnboundedReceiver<Wake>;

/// The GPUI-side wakeup handle. Content wakes coalesce until the pane renders;
/// chrome wakes bypass that pending bit because a background pane may not render
/// but its tab and workspace indicators must still update.
#[derive(Clone)]
pub(crate) struct WakeSignal {
    queued: Arc<AtomicBool>,
    resignal: Arc<AtomicBool>,
    tx: UnboundedSender<Wake>,
}

pub(crate) fn wake_channel() -> (WakeSignal, WakeReceiver) {
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
    pub(crate) fn signal(&self, wake: Wake) -> bool {
        if matches!(wake, Wake::Chrome(_)) {
            let _ = self.tx.unbounded_send(wake);
            return true;
        }
        if self.queued.swap(true, Ordering::AcqRel) {
            self.resignal.store(true, Ordering::Release);
            return false;
        }
        let _ = self.tx.unbounded_send(wake);
        true
    }

    pub(crate) fn mark_delivered(&self, surface_id: u64) {
        self.queued.store(false, Ordering::Release);
        if self.resignal.swap(false, Ordering::AcqRel) {
            self.signal(Wake::Content(surface_id));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::Mutex;

    use super::{Wake, WakeSender, wake_channel};

    #[test]
    fn callback_wake_sender_forwards_wake() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_for_sender = Arc::clone(&seen);
        let sender = WakeSender::from_fn(move |wake| {
            seen_for_sender.lock().push(wake);
        });

        sender.send(Wake::Content(7));

        assert_eq!(*seen.lock(), vec![Wake::Content(7)]);
    }

    #[test]
    fn coalesces_until_delivered() {
        let (wake, mut rx) = wake_channel();

        assert!(wake.signal(Wake::Content(7)));
        assert!(!wake.signal(Wake::Content(7)));
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err());

        wake.mark_delivered(7);
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err());
        wake.mark_delivered(7);
        assert!(wake.signal(Wake::Content(7)));
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn inactive_content_cannot_suppress_chrome() {
        let (wake, mut rx) = wake_channel();

        assert!(wake.signal(Wake::Content(7)));
        assert_eq!(rx.try_recv(), Ok(Wake::Content(7)));
        assert!(!wake.signal(Wake::Content(7)));
        assert!(rx.try_recv().is_err());
        assert!(wake.signal(Wake::Chrome(7)));
        assert_eq!(rx.try_recv(), Ok(Wake::Chrome(7)));
    }
}
