mod parse;
mod preview;
mod render;
mod view;

#[cfg(test)]
mod tests;

use std::collections::HashMap;

use gpui::prelude::*;
use gpui::{Context, Entity, Subscription, WeakEntity, div};
use gpui_component::text::TextView;

use crate::agent_tab::transcript::TranscriptView;
use crate::agent_tab::transcript::mermaid::parse::DiagramSource;
use crate::agent_tab::transcript::mermaid::view::DiagramView;
use crate::agent_tab::transcript::rows::RowSpec;
use crate::agent_tab::transcript::text::TextPart;

const MAX_ENTRIES: usize = 32;
const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct DiagramKey {
    entry: usize,
    part: TextPart,
    offset: usize,
}

struct CachedDiagram {
    view: Entity<DiagramView>,
    used: u64,
}

struct VisibleDiagram {
    view: WeakEntity<DiagramView>,
    _observation: Subscription,
}

pub(super) struct MermaidCache {
    owner: WeakEntity<TranscriptView>,
    entries: HashMap<DiagramKey, CachedDiagram>,
    visible: HashMap<DiagramKey, VisibleDiagram>,
    clock: u64,
}

impl MermaidCache {
    pub(super) fn new(owner: WeakEntity<TranscriptView>) -> Self {
        Self {
            owner,
            entries: HashMap::new(),
            visible: HashMap::new(),
            clock: 0,
        }
    }

    pub(super) fn extend(
        view: TextView,
        cache: WeakEntity<Self>,
        entry: usize,
        part: TextPart,
    ) -> TextView {
        view.markdown_block_parser(parse::parse)
            .markdown_block_renderer("mermaid", move |node, _, cx| {
                let Some(source) = node.data::<DiagramSource>() else {
                    return div().into_any_element();
                };

                let key = DiagramKey {
                    entry,
                    part,
                    offset: source.offset,
                };

                cache
                    .update(cx, |cache, cx| cache.ensure(key, &source.code, cx))
                    .map(IntoElement::into_any_element)
                    .unwrap_or_else(|_| div().child(node.as_text().to_string()).into_any_element())
            })
    }

    fn ensure(
        &mut self,
        key: DiagramKey,
        source: &str,
        cx: &mut Context<Self>,
    ) -> Entity<DiagramView> {
        self.clock += 1;

        if let Some(cached) = self.entries.get_mut(&key)
            && cached.view.read(cx).source() == source
        {
            cached.used = self.clock;

            return cached.view.clone();
        }

        // Visible elements retain their entities after cache eviction. Reuse them
        // so a reply with many diagrams does not restart rendering every frame.
        self.visible
            .retain(|_, cached| cached.view.upgrade().is_some());

        let view = self
            .visible
            .get(&key)
            .and_then(|cached| cached.view.upgrade())
            .filter(|view| view.read(cx).source() == source)
            .unwrap_or_else(|| cx.new(|cx| DiagramView::new(source.to_owned(), cx)));

        let owner = self.owner.clone();

        let observation = cx.observe(&view, move |cache, _, cx| {
            cache.trim(key, cx);

            let _ = owner.update(cx, |transcript, cx| {
                if let Some(row) = transcript.rows.iter().position(|row| {
                    matches!(row.spec,
                        RowSpec::Entry { index, .. } | RowSpec::Work { index, .. }
                        if index == key.entry)
                }) {
                    transcript.transcript_list.remeasure_items(row..row + 1);
                }

                cx.notify();
            });
        });

        self.visible.insert(
            key,
            VisibleDiagram {
                view: view.downgrade(),
                _observation: observation,
            },
        );

        self.entries.insert(
            key,
            CachedDiagram {
                view: view.clone(),
                used: self.clock,
            },
        );

        self.trim(key, cx);

        view
    }

    fn trim(&mut self, keep: DiagramKey, cx: &mut Context<Self>) {
        loop {
            let bytes: usize = self
                .entries
                .values()
                .map(|cached| cached.view.read(cx).image_bytes())
                .sum();

            if self.entries.len() <= MAX_ENTRIES && bytes <= MAX_IMAGE_BYTES {
                break;
            }

            let oldest = self
                .entries
                .iter()
                .filter(|(key, _)| **key != keep)
                .min_by_key(|(_, cached)| cached.used)
                .map(|(key, _)| *key);

            let Some(oldest) = oldest else {
                break;
            };

            self.entries.remove(&oldest);
        }
    }
}
