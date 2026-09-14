use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use nmt_agent::background_task::BackgroundTaskKey;

pub(super) type ChildReaders = Rc<RefCell<HashMap<(u64, BackgroundTaskKey), usize>>>;

/// Refresh interest ends with the reader and never retains the backend.
pub struct ChildReader {
    pub(super) key: (u64, BackgroundTaskKey),
    pub(super) readers: ChildReaders,
}

impl Drop for ChildReader {
    fn drop(&mut self) {
        let mut readers = self.readers.borrow_mut();

        if let Some(count) = readers.get_mut(&self.key) {
            *count -= 1;

            if *count == 0 {
                readers.remove(&self.key);
            }
        }
    }
}
