#[cfg(test)]
#[path = "children_tests.rs"]
mod tests;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::background_task::{
    BackgroundTaskKey, BackgroundTaskLoadState, BackgroundTaskSnapshot,
    BackgroundTaskTranscriptUpdate, MAX_TRANSCRIPT_ITEMS,
};
use crate::session::{AgentKind, RecoveryIdentity};
use crate::transcript::conversation::ConversationState;

/// Child-agent activity the provider adapter reports for this conversation.
#[derive(Default)]
pub struct ChildAgents {
    /// Latest child-agent snapshot published by the provider adapter. The
    /// adapter owns child lifecycle; the pane keeps only this replacement
    /// copy so the right-side view never maintains a second mutable registry.
    pub background_tasks: Option<BackgroundTaskSnapshot>,

    /// Each child's own conversation, accumulated here rather than in the
    /// adapter so live activity is retained once and the retention bound
    /// applies to what is actually shown.
    pub transcripts: HashMap<BackgroundTaskKey, ChildTranscript>,

    /// Claude session id whose child agents were already restored from
    /// history. Ready fires again during first-turn initialization, so the
    /// read happens once per conversation rather than once per confirmation.
    pub restored_session: Option<String>,
}

/// Show a task snapshot only against the parent session it was produced for.
/// Provider adapters publish snapshots asynchronously, so a snapshot can still
/// be held when the pane has already moved to another session or has no
/// session id yet; in both cases the view must render nothing rather than
/// another conversation's children.
pub fn scoped_background_tasks<'a>(
    parent: Option<&BackgroundTaskKey>,
    snapshot: Option<&'a BackgroundTaskSnapshot>,
) -> Option<&'a BackgroundTaskSnapshot> {
    let parent = parent?;
    let snapshot = snapshot?;

    (&snapshot.parent_session == parent).then_some(snapshot)
}

impl ChildAgents {
    /// The held snapshot, when it was produced for `parent`.
    pub fn scoped(&self, parent: Option<&BackgroundTaskKey>) -> Option<&BackgroundTaskSnapshot> {
        scoped_background_tasks(parent, self.background_tasks.as_ref())
    }

    /// Child `key`'s conversation, withheld with the snapshot when that
    /// snapshot belongs to a session other than `parent`.
    pub fn transcript(
        &self,
        parent: Option<&BackgroundTaskKey>,
        key: &BackgroundTaskKey,
    ) -> Option<&ChildTranscript> {
        self.scoped(parent)?;

        self.transcripts.get(key)
    }

    /// The children `parent` shows, and how many of them are still active.
    pub fn activity(&self, parent: Option<&BackgroundTaskKey>) -> (usize, usize) {
        self.scoped(parent)
            .map_or((0, 0), |tasks| (tasks.tasks.len(), tasks.active_count()))
    }

    /// Take a replacement snapshot, reporting whether the activity `parent`
    /// shows changed. The chrome shows those counts, so it is told on a
    /// change rather than on every republished snapshot.
    pub(crate) fn set_snapshot(
        &mut self,
        parent: Option<&BackgroundTaskKey>,
        snapshot: BackgroundTaskSnapshot,
    ) -> bool {
        let before = self.activity(parent);

        self.background_tasks = Some(snapshot);

        self.activity(parent) != before
    }

    /// Apply `update` to child `key`'s conversation, starting one for a child
    /// not seen before. Returns whether the conversation changed.
    pub(crate) fn apply_transcript(
        &mut self,
        key: BackgroundTaskKey,
        update: BackgroundTaskTranscriptUpdate,
    ) -> bool {
        self.transcripts.entry(key).or_default().apply(update)
    }

    /// Drop the snapshot and every child conversation. Readers may still
    /// hold a child's shared conversation, so each is emptied in place too.
    pub(crate) fn clear(&mut self) {
        self.background_tasks = None;

        for child in self.transcripts.values() {
            child.conversation.borrow_mut().clear();
        }

        self.transcripts.clear();
    }

    pub fn claim_restore(&mut self, session_id: &str) -> bool {
        if self.restored_session.as_deref() == Some(session_id) {
            return false;
        }

        self.restored_session = Some(session_id.to_owned());

        true
    }

    pub fn parent(identity: RecoveryIdentity) -> Option<BackgroundTaskKey> {
        Some(match identity.kind {
            AgentKind::Codex => BackgroundTaskKey::codex(identity.id),
            AgentKind::Claude => BackgroundTaskKey::claude_code(identity.id),
            AgentKind::DeepSeek => return None,
        })
    }
}

/// Retained child content is shared with readers without copying its items.
#[derive(Default)]
pub struct ChildTranscript {
    pub conversation: Rc<RefCell<ConversationState>>,
    state: BackgroundTaskLoadState,
    dropped: usize,
}

impl ChildTranscript {
    pub fn state(&self) -> &BackgroundTaskLoadState {
        &self.state
    }

    pub fn dropped(&self) -> usize {
        self.dropped
    }

    pub fn apply(&mut self, update: BackgroundTaskTranscriptUpdate) -> bool {
        let mut changed = false;

        if let Some(state) = update.state
            && self.state != state
        {
            self.state = state;
            changed = true;
        }

        let mut conversation = self.conversation.borrow_mut();

        if update.restore && !conversation.content.entries().is_empty() {
            return changed;
        }

        if update.replace || update.restore {
            if conversation
                .content
                .entries()
                .iter()
                .map(|entry| &entry.item)
                .eq(update.items.iter())
            {
                return changed;
            }

            conversation.clear();

            self.dropped = 0;
            changed = true;
        }

        for item in update.items {
            if item
                .id()
                .is_some_and(|id| conversation.content.contains_item(id))
            {
                conversation.merge_completed(&item);
            } else {
                conversation.push(0, item, Vec::new());
            }

            changed = true;
        }

        let dropped = conversation.retain_last(MAX_TRANSCRIPT_ITEMS);

        if dropped > 0 {
            self.dropped += dropped;
        }

        changed
    }
}
