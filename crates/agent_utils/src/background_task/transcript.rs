//! One child agent's conversation, in the same backend-neutral items the
//! parent conversation uses. Providers deliver these differently — Codex reads
//! a stored descendant thread in one response, Claude Code streams linked
//! activity as it happens — so the shared piece is the accumulator, not the
//! loading.

use crate::chat::Item;

/// Items retained per child. A long-running child can emit an unbounded
/// number, and several children can be open across a session, so the oldest
/// are dropped rather than letting one child grow without limit.
pub const MAX_TRANSCRIPT_ITEMS: usize = 512;

/// How far a child's conversation has been loaded. Kept apart from the items
/// so a failed load can report itself without discarding what is already known.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum BackgroundTaskTranscriptState {
    #[default]
    NotLoaded,
    Loading,
    Ready,
    Unavailable {
        message: String,
    },
}

/// A child's conversation as the view reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BackgroundTaskTranscript {
    items: Vec<Item>,
    state: BackgroundTaskTranscriptState,
    /// Items dropped from the front to stay within the retention bound. The
    /// view reports this rather than presenting a truncated conversation as
    /// though it were complete.
    dropped: usize,
    /// Bumped by every change. A view syncing from this compares one integer
    /// instead of the whole conversation, which matters because the comparison
    /// would otherwise run per frame over long outputs.
    revision: u64,
}

impl BackgroundTaskTranscript {
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn state(&self) -> &BackgroundTaskTranscriptState {
        &self.state
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// How many items were dropped at the retention bound; zero when the whole
    /// conversation is present.
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn set_state(&mut self, state: BackgroundTaskTranscriptState) -> bool {
        if self.state == state {
            return false;
        }
        self.state = state;
        self.revision += 1;
        true
    }

    /// Append one item, or fold it into the entry that streamed it. An item
    /// carrying the same id is a completion of that entry rather than a new
    /// row, which is the same rule the parent conversation applies.
    pub fn push(&mut self, item: Item) {
        self.revision += 1;
        if let Some(id) = item.id()
            && let Some(existing) = self
                .items
                .iter_mut()
                .find(|existing| existing.id() == Some(id))
        {
            existing.merge_completed(&item);
            return;
        }

        self.items.push(item);
        if self.items.len() > MAX_TRANSCRIPT_ITEMS {
            let excess = self.items.len() - MAX_TRANSCRIPT_ITEMS;
            self.items.drain(..excess);
            self.dropped += excess;
        }
    }

    pub fn extend(&mut self, items: impl IntoIterator<Item = Item>) {
        for item in items {
            self.push(item);
        }
    }

    /// Take a provider's complete read of the conversation. Used when the
    /// provider can supply the whole thing at once; it discards any dropped
    /// count because nothing is missing from the new content.
    pub fn replace(&mut self, items: Vec<Item>) {
        self.revision += 1;
        self.dropped = 0;
        self.items.clear();
        self.extend(items);
    }

    /// Fold a restored conversation in. History predates whatever the live
    /// stream produced, so it only fills a child nothing has been seen for;
    /// otherwise the live content is newer and is kept.
    pub fn restore(&mut self, items: Vec<Item>) -> bool {
        if !self.items.is_empty() {
            return false;
        }
        self.replace(items);
        true
    }
}

/// One provider update to a child's conversation.
#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundTaskTranscriptUpdate {
    /// Whether `items` is the provider's complete read rather than new
    /// activity to append.
    pub replace: bool,
    /// Whether `items` come from persisted history, which only fills a child
    /// whose conversation has not been seen live.
    pub restore: bool,
    pub items: Vec<Item>,
    pub state: Option<BackgroundTaskTranscriptState>,
}

impl BackgroundTaskTranscriptUpdate {
    /// New activity observed on a live stream.
    pub fn appended(items: Vec<Item>) -> Self {
        Self {
            replace: false,
            restore: false,
            items,
            state: Some(BackgroundTaskTranscriptState::Ready),
        }
    }

    /// A provider's complete read of a stored conversation.
    pub fn loaded(items: Vec<Item>) -> Self {
        Self {
            replace: true,
            restore: false,
            items,
            state: Some(BackgroundTaskTranscriptState::Ready),
        }
    }

    /// A conversation rebuilt from persisted history. Applied only to a child
    /// nothing has been seen for, so older content cannot replace newer live
    /// activity.
    pub fn restored(items: Vec<Item>) -> Self {
        Self {
            replace: false,
            restore: true,
            items,
            state: Some(BackgroundTaskTranscriptState::Ready),
        }
    }

    pub fn state(state: BackgroundTaskTranscriptState) -> Self {
        Self {
            replace: false,
            restore: false,
            items: Vec::new(),
            state: Some(state),
        }
    }

    /// Returns whether anything changed, so a caller only repaints for a real
    /// update.
    pub fn apply_to(self, transcript: &mut BackgroundTaskTranscript) -> bool {
        let mut changed = false;
        if let Some(state) = self.state {
            changed |= transcript.set_state(state);
        }
        if self.restore {
            changed |= transcript.restore(self.items);
        } else if self.replace {
            if transcript.items() != self.items {
                transcript.replace(self.items);
                changed = true;
            }
        } else if !self.items.is_empty() {
            transcript.extend(self.items);
            changed = true;
        }
        changed
    }
}
