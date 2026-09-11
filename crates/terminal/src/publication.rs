use std::mem;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::render_buffer::RenderBuffer;

/// Readers retain an immutable frame after releasing the publication lock.
/// Parsing, extraction, and destruction of old frames never hold this lock.
pub struct FrameStore {
    state: Mutex<PublishedFrames>,
}

struct PublishedFrames {
    current: Arc<RenderBuffer>,
    retired: Vec<Arc<RenderBuffer>>,
}

impl FrameStore {
    pub fn new(frame: RenderBuffer) -> Self {
        Self {
            state: Mutex::new(PublishedFrames {
                current: Arc::new(frame),
                retired: Vec::with_capacity(4),
            }),
        }
    }

    pub fn load(&self) -> Arc<RenderBuffer> {
        Arc::clone(&self.state.lock().current)
    }

    pub(crate) fn publish(&self, back: &mut RenderBuffer) {
        {
            let mut state = self.state.lock();

            // Holding the publication lock prevents new readers from loading
            // the frame while unique ownership permits exchanging its contents.
            if let Some(front) = Arc::get_mut(&mut state.current) {
                mem::swap(front, back);
                return;
            }

            for (index, frame) in state.retired.iter_mut().enumerate() {
                if let Some(frame) = Arc::get_mut(frame) {
                    mem::swap(frame, back);
                    let next = state.retired.swap_remove(index);
                    let old = mem::replace(&mut state.current, next);
                    state.retired.push(old);
                    return;
                }
            }
        }

        // Allocate only when readers still retain every reusable frame. The
        // allocation and any evicted frame's destruction stay outside the lock.
        let next = mem::replace(back, RenderBuffer::new(0, 0));
        let next = Arc::new(next);
        let discarded = {
            let mut state = self.state.lock();
            let old = mem::replace(&mut state.current, next);
            state.retired.push(old);
            (state.retired.len() > 3).then(|| state.retired.remove(0))
        };
        drop(discarded);
    }
}
