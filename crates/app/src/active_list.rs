//! Ordered, never-empty list with a stable-id active element.
//!
//! Shared engine behind `TabManager` and `WorkspaceManager`: both maintain a
//! `Vec` plus an active index where identity is a stable id that survives
//! close and reorder. The close-neighbor rule, activation bounds, and the
//! "active follows its element by id" invariant live here once.

/// Stable identity for elements of an [`ActiveList`].
pub trait HasId {
    type Id: Copy + PartialEq;
    fn id(&self) -> Self::Id;
}

/// Invariant: never empty — `close` refuses the last element, so `active`
/// always points at a real element.
pub struct ActiveList<T: HasId> {
    items: Vec<T>,
    active: usize,
}

impl<T: HasId> ActiveList<T> {
    /// Start with a single active element. There is no empty state.
    pub fn new(first: T) -> Self {
        Self {
            items: vec![first],
            active: 0,
        }
    }

    /// Append an element and make it active.
    pub fn push_active(&mut self, item: T) {
        self.items.push(item);
        self.active = self.items.len() - 1;
    }

    /// Remove the element with `id`, returning it. Refuses the last element
    /// (`None`). After removing the active element the active falls to the
    /// right neighbor, or the left when there is no right neighbor.
    pub fn close(&mut self, id: T::Id) -> Option<T> {
        if self.items.len() <= 1 {
            return None;
        }
        let idx = self.index_of(id)?;
        let removed = self.items.remove(idx);
        if idx < self.active {
            self.active -= 1;
        } else if idx == self.active {
            // idx now points at the former right neighbor; clamp to the left
            // neighbor when the removed element was last.
            self.active = idx.min(self.items.len() - 1);
        }
        Some(removed)
    }

    /// Activate by position. Out-of-range indices are ignored.
    pub fn activate(&mut self, index: usize) {
        if index < self.items.len() {
            self.active = index;
        }
    }

    pub fn focus_next(&mut self) {
        self.active = (self.active + 1) % self.items.len();
    }

    pub fn focus_prev(&mut self) {
        let n = self.items.len();
        self.active = (self.active + n - 1) % n;
    }

    /// Move the element at `from` to position `to`, keeping the same element
    /// active. No-op for out-of-range or equal indices.
    pub fn reorder(&mut self, from: usize, to: usize) {
        let n = self.items.len();
        if from >= n || to >= n || from == to {
            return;
        }
        self.edit_preserving_active(|items| {
            let item = items.remove(from);
            items.insert(to, item);
        });
    }

    /// Run an order-mutating edit on the underlying vec while the active
    /// element (tracked by id) stays active. The edit must keep the list
    /// non-empty and must not remove the active element.
    pub fn edit_preserving_active(&mut self, edit: impl FnOnce(&mut Vec<T>)) {
        let active_id = self.items[self.active].id();
        edit(&mut self.items);
        self.active = self.index_of(active_id).unwrap_or(self.active);
    }

    pub fn active(&self) -> &T {
        &self.items[self.active]
    }

    pub fn active_mut(&mut self) -> &mut T {
        &mut self.items[self.active]
    }

    pub fn active_id(&self) -> T::Id {
        self.items[self.active].id()
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn items(&self) -> &[T] {
        &self.items
    }

    pub fn items_mut(&mut self) -> &mut [T] {
        &mut self.items
    }

    /// Find an element by id. Index may have shifted; id is stable.
    pub fn find(&self, id: T::Id) -> Option<&T> {
        self.index_of(id).map(|idx| &self.items[idx])
    }

    pub fn find_mut(&mut self, id: T::Id) -> Option<&mut T> {
        self.index_of(id).map(|idx| &mut self.items[idx])
    }

    pub fn index_of(&self, id: T::Id) -> Option<usize> {
        self.items.iter().position(|item| item.id() == id)
    }
}
