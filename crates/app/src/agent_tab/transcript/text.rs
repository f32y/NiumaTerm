use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

#[cfg(test)]
use gpui::EntityId;
use gpui::{AppContext as _, Context, Entity, ScrollHandle, Subscription};
use gpui_base::text::TextFragment;
use gpui_base::text::search::TextSearch;
use gpui_component::text::{TextView, TextViewState};

use crate::agent_tab::transcript::TranscriptView;
use crate::agent_tab::transcript::mermaid::MermaidCache;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum TextPart {
    Reply,
    Work,
    Summary,
    Instructions,
    Message,
    Prompt,
    Annotation(usize),
    Error,
}

/// Parsed text must outlast the elements a virtual list creates for each
/// frame. Recreating it on return would measure an empty async document
/// in place of a previously visible reply.
#[derive(Default)]
pub(super) struct TranscriptTextCache {
    pub(super) search: HashMap<(usize, TextPart), TextSearch>,
    pub(super) search_scrolls: HashMap<(usize, TextPart), ScrollHandle>,
    entries: HashMap<(usize, TextPart), CachedText>,
    entry_count: usize,
    mermaid: Option<Entity<MermaidCache>>,
}

struct CachedText {
    state: Entity<TextViewState>,
    _observation: Subscription,
    blocks: Vec<TextFragment>,
    fragments: HashMap<usize, (Entity<TextViewState>, u64)>,
    clock: u64,
    #[cfg(test)]
    created_fragments: usize,
}

impl TranscriptTextCache {
    #[cfg(test)]
    pub(super) fn fragment_ids(&self, index: usize) -> Vec<EntityId> {
        let mut ids = self.entries[&(index, TextPart::Reply)]
            .fragments
            .values()
            .map(|(state, _)| state.entity_id())
            .collect::<Vec<_>>();

        ids.sort_unstable();

        ids
    }

    #[cfg(test)]
    pub(super) fn fragment_creations(&self, index: usize) -> usize {
        self.entries[&(index, TextPart::Reply)].created_fragments
    }

    pub(super) fn reply_state(&self, index: usize) -> Option<Entity<TextViewState>> {
        self.entries
            .get(&(index, TextPart::Reply))
            .map(|cached| cached.state.clone())
    }

    pub(super) fn fragments(&self, index: usize) -> &[TextFragment] {
        self.entries
            .get(&(index, TextPart::Reply))
            .map_or(&[], |cached| &cached.blocks)
    }

    fn refresh_fragments(&mut self, index: usize, cx: &mut Context<TranscriptView>) {
        let Some(cached) = self.entries.get_mut(&(index, TextPart::Reply)) else {
            return;
        };

        cached.blocks = cached.state.update(cx, |state, _| state.fragments());
    }

    pub(super) fn fragment_view(
        &mut self,
        index: usize,
        block: usize,
        cx: &mut Context<TranscriptView>,
    ) -> TextView {
        let owner = cx.entity().downgrade();

        let mermaid = self
            .mermaid
            .get_or_insert_with(|| cx.new(|_| MermaidCache::new(owner)))
            .downgrade();

        let cached = self
            .entries
            .get_mut(&(index, TextPart::Reply))
            .expect("fragment row has a parsed document");

        let fragment = cached.blocks[block].clone();

        cached.clock += 1;

        let state = if let Some((state, used)) = cached.fragments.get_mut(&block) {
            *used = cached.clock;

            state.update(cx, |state, cx| state.set_fragment(fragment, cx));

            state.clone()
        } else {
            #[cfg(test)]
            {
                cached.created_fragments += 1;
            }

            let state = cx.new(|cx| TextViewState::from_fragment(fragment, cx));

            cached
                .fragments
                .insert(block, (state.clone(), cached.clock));

            state
        };

        while cached.fragments.len() > 64 {
            let oldest = cached
                .fragments
                .iter()
                .filter(|(at, (state, _))| {
                    **at != block && !state.read(cx).retains_selection_state(cx)
                })
                .min_by_key(|(_, (_, used))| *used)
                .map(|(at, _)| *at);

            let Some(oldest) = oldest else { break };

            cached.fragments.remove(&oldest);
        }

        MermaidCache::extend(
            TextView::new(&state).search(self.search.get(&(index, TextPart::Reply)).cloned()),
            mermaid,
            index,
            TextPart::Reply,
        )
    }

    pub(super) fn view(
        &mut self,
        index: usize,
        part: TextPart,
        text: &str,
        cx: &mut Context<TranscriptView>,
    ) -> TextView {
        let mermaid = if matches!(
            part,
            TextPart::Prompt | TextPart::Annotation(_) | TextPart::Error
        ) {
            None
        } else {
            let owner = cx.entity().downgrade();

            Some(
                self.mermaid
                    .get_or_insert_with(|| cx.new(|_| MermaidCache::new(owner)))
                    .clone(),
            )
        };

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
                    TextPart::Prompt | TextPart::Annotation(_) | TextPart::Error => {
                        TextViewState::plain(text, cx)
                    }
                    _ => TextViewState::markdown(text, cx),
                });

                if let Some(mermaid) = &mermaid {
                    let extensions =
                        Arc::new(MermaidCache::extensions(mermaid.downgrade(), index, part));

                    state.update(cx, |state, cx| {
                        state.set_markdown_extensions(extensions, cx)
                    });
                }

                let mut revision = state.read(cx).content_revision();

                let observation = cx.observe(&state, move |transcript, state, cx| {
                    let current = state.read(cx).content_revision();

                    if current == revision {
                        return;
                    }

                    revision = current;

                    if part == TextPart::Reply {
                        transcript.text_views.refresh_fragments(index, cx);
                        transcript.row_cache.invalidate(index);
                    }

                    if part != TextPart::Reply || transcript.text_views.fragments(index).is_empty()
                    {
                        transcript.measurements.invalidate(index);
                    }

                    cx.notify();
                });

                entry.insert(CachedText {
                    state,
                    _observation: observation,
                    blocks: Vec::new(),
                    fragments: HashMap::new(),
                    clock: 0,
                    #[cfg(test)]
                    created_fragments: 0,
                })
            }
        };

        let view = TextView::new(&cached.state).search(self.search.get(&(index, part)).cloned());
        let initialize_blocks = part == TextPart::Reply && cached.clock == 0;

        if initialize_blocks {
            cached.clock = 1;
        }

        if matches!(
            part,
            TextPart::Prompt | TextPart::Annotation(_) | TextPart::Error
        ) {
            return view;
        }

        let mermaid = mermaid.expect("Markdown text initializes diagram cache");

        let view = MermaidCache::extend(view, mermaid.downgrade(), index, part);

        if initialize_blocks {
            self.refresh_fragments(index, cx);

            if !self.fragments(index).is_empty() {
                cx.notify();

                let owner = cx.entity().downgrade();

                cx.defer(move |cx| {
                    let _ = owner.update(cx, |view, cx| {
                        view.row_cache.invalidate(index);

                        cx.notify();
                    });
                });
            }
        }

        view
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.search.clear();
        self.search_scrolls.clear();

        self.mermaid = None;

        self.entry_count = 0;
    }

    pub(super) fn sync_entry_count(&mut self, len: usize) {
        if len < self.entry_count {
            self.entries.retain(|(index, _), _| *index < len);

            self.mermaid = None;
        }

        self.entry_count = len;
    }

    pub(super) fn drop_row(&mut self, index: usize) {
        self.entries.retain(|(entry, _), _| *entry != index);
    }
}
