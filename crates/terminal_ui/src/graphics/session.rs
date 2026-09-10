use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use nmt_terminal::clipboard::{Clipboard, ClipboardType};
use nmt_terminal::event::BlockEvent;
use nmt_terminal::graphics::UpdateQueues;
use nmt_terminal::session::{SessionChange, SessionObserver};
use parking_lot::Mutex;

use crate::graphics::{FrozenImageCache, GenerationStore, prune_frozen_images};
use crate::wake::{Wake, WakeSender};

pub(crate) struct SessionImages {
    pub(crate) generations: Mutex<GenerationStore>,
    pub(crate) frozen: FrozenImageCache,
    live_count: AtomicUsize,
    id: u64,
    wake: Option<WakeSender>,
}

impl SessionImages {
    pub(crate) fn new(id: u64, wake: Option<WakeSender>) -> Self {
        Self {
            generations: Mutex::new(GenerationStore::default()),
            frozen: Arc::default(),
            live_count: AtomicUsize::new(0),
            id,
            wake,
        }
    }

    pub(crate) fn has_live_images(&self) -> bool {
        self.live_count.load(Ordering::Relaxed) != 0
    }
}

impl SessionObserver for SessionImages {
    fn graphics(&self, updates: UpdateQueues) {
        let mut store = self.generations.lock();

        for (id, data) in updates.pending_images {
            store.install(id, data);
        }

        for id in updates.remove_queue {
            store.remove(id.0 as u32);
        }

        self.live_count.store(store.len(), Ordering::Relaxed);
    }

    fn blocks(&self, events: &[BlockEvent]) {
        prune_frozen_images(&self.frozen, events);
    }

    fn clipboard(&self, kind: ClipboardType, text: String) {
        Clipboard::default().set(kind, text);
    }

    fn changed(&self, change: SessionChange) {
        if let Some(wake) = &self.wake {
            wake.send(match change {
                SessionChange::Content => Wake::Content(self.id),
                SessionChange::HostEvents => Wake::Chrome(self.id),
            });
        }
    }
}
