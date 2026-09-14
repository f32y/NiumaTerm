use std::collections::HashMap;
use std::hash::Hash;
use std::mem;

pub(crate) struct PendingRequests<Id, Op> {
    next_id: u64,
    pub(crate) operations: HashMap<Id, Op>,
    closed: bool,
}

impl<Id: Eq + Hash, Op> PendingRequests<Id, Op> {
    pub(crate) fn new(next_id: u64) -> Self {
        Self {
            next_id,
            operations: HashMap::new(),
            closed: false,
        }
    }

    pub(crate) fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;

        self.next_id += 1;

        id
    }

    pub(crate) fn track(&mut self, id: Id, operation: Op) {
        if !self.closed {
            self.operations.insert(id, operation);
        }
    }

    pub(crate) fn finish(&mut self, id: &Id) -> Option<Op> {
        self.operations.remove(id)
    }

    pub(crate) fn close(&mut self) -> HashMap<Id, Op> {
        self.closed = true;

        mem::take(&mut self.operations)
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed
    }

    #[cfg(test)]
    pub(crate) fn next_id(&self) -> u64 {
        self.next_id
    }
}
