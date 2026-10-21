use std::collections::HashMap;
use std::collections::hash_map::Entry;

use gpui::{AppContext as _, Context, Entity, Subscription};
use gpui_component::text::{TextView, TextViewState};

use crate::agent_tab::transcript::TranscriptView;
use crate::agent_tab::transcript::rows::RowSpec;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum TextPart {
    Reply,
    Work,
    Summary,
    Instructions,
    Message,
    Prompt,
    Annotation(usize),
}

/// Parsed text must outlast the elements a virtual list creates for each
/// frame. Recreating it on return would measure an empty async document
/// in place of a previously visible reply.
#[derive(Default)]
pub(super) struct TranscriptTextCache {
    entries: HashMap<(usize, TextPart), CachedText>,
    entry_count: usize,
}

struct CachedText {
    state: Entity<TextViewState>,
    _observation: Subscription,
}

impl TranscriptTextCache {
    pub(super) fn view(
        &mut self,
        index: usize,
        part: TextPart,
        text: &str,
        cx: &mut Context<TranscriptView>,
    ) -> TextView {
        let cached = match self.entries.entry((index, part)) {
            Entry::Occupied(entry) => {
                let cached = entry.into_mut();

                cached
                    .state
                    .update(cx, |state, cx| state.set_text(text, cx));

                cached
            }
            Entry::Vacant(entry) => {
                let state = cx.new(|cx| match part {
                    TextPart::Prompt | TextPart::Annotation(_) => TextViewState::plain(text, cx),
                    _ => TextViewState::markdown(text, cx),
                });

                let observation = cx.observe(&state, move |transcript, _, cx| {
                    // Parsing completes independently of content updates.
                    // Invalidate the matching list measurement even when
                    // that row is currently outside the viewport.
                    if let Some(row) = transcript.rows.iter().position(|row| {
                        matches!(row.spec,
                            RowSpec::Entry { index: entry, .. } | RowSpec::Work { index: entry, .. }
                            if entry == index)
                    }) {
                        transcript.transcript_list.remeasure_items(row..row + 1);
                    }

                    cx.notify();
                });

                entry.insert(CachedText {
                    state,
                    _observation: observation,
                })
            }
        };

        TextView::new(&cached.state)
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();

        self.entry_count = 0;
    }

    pub(super) fn sync_entry_count(&mut self, len: usize) {
        if len < self.entry_count {
            self.entries.retain(|(index, _), _| *index < len);
        }

        self.entry_count = len;
    }

    pub(super) fn drop_row(&mut self, index: usize) {
        self.entries.retain(|(entry, _), _| *entry != index);
    }
}
