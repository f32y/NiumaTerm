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

/// How far a provider read has got: the child list of a session, or one
/// child's conversation. Kept apart from the data so a failed load can report
/// itself without discarding what is already known.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum BackgroundTaskLoadState {
    #[default]
    NotLoaded,
    Loading,
    Ready,
    Unavailable {
        message: String,
    },
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
    pub state: Option<BackgroundTaskLoadState>,
}

impl BackgroundTaskTranscriptUpdate {
    /// New activity observed on a live stream.
    pub fn appended(items: Vec<Item>) -> Self {
        Self {
            replace: false,
            restore: false,
            items,
            state: Some(BackgroundTaskLoadState::Ready),
        }
    }

    /// A provider's complete read of a stored conversation.
    pub fn loaded(items: Vec<Item>) -> Self {
        Self {
            replace: true,
            restore: false,
            items,
            state: Some(BackgroundTaskLoadState::Ready),
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
            state: Some(BackgroundTaskLoadState::Ready),
        }
    }

    pub fn state(state: BackgroundTaskLoadState) -> Self {
        Self {
            replace: false,
            restore: false,
            items: Vec::new(),
            state: Some(state),
        }
    }
}
