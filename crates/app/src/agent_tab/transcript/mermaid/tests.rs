use gpui::prelude::*;
use gpui::{Context, Entity, TestAppContext, Window, div};
use gpui_component::text::{TextView, TextViewState};

use crate::agent_tab::AgentKind;
use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::transcript::TranscriptView;
use crate::agent_tab::transcript::mermaid::{DiagramKey, MAX_ENTRIES, MermaidCache};
use crate::agent_tab::transcript::text::TextPart;

struct MarkdownProbe {
    owner: Entity<TranscriptView>,
    cache: Entity<MermaidCache>,
    text: Entity<TextViewState>,
}

impl Render for MarkdownProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w_full().child(
            MermaidCache::extend(
                TextView::new(&self.text),
                self.cache.downgrade(),
                0,
                TextPart::Reply,
            )
            .selectable(true),
        )
    }
}

#[gpui::test]
async fn streamed_markdown_renders_after_closing_and_notifies_the_transcript(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    cx.set_global(AgentSettings::default());

    let source = "Before\n\n```mermaid\nflowchart TD\nA --> B\n";

    let (probe, cx) = cx.add_window_view(|_, cx| {
        let owner = cx.new(|_| TranscriptView::new(AgentKind::Codex, None));
        let cache = cx.new(|_| MermaidCache::new(owner.downgrade()));
        let text = cx.new(|cx| TextViewState::markdown(source, cx));

        MarkdownProbe { owner, cache, text }
    });

    cx.run_until_parked();
    cx.refresh().unwrap();

    let (owner, cache, text) = probe.read_with(cx, |probe, _| {
        (probe.owner.clone(), probe.cache.clone(), probe.text.clone())
    });

    cache.read_with(cx, |cache, _| assert!(cache.entries.is_empty()));

    text.update(cx, |text, cx| text.set_text(&format!("{source}```"), cx));

    cx.condition(&owner, |_, cx| {
        cache
            .read(cx)
            .entries
            .values()
            .any(|entry| entry.view.read(cx).image_bytes() > 0)
    })
    .await;

    cx.refresh().unwrap();

    assert!(cx.debug_bounds("mermaid-preview").is_some());

    let original = cache.read_with(cx, |cache, _| {
        cache.entries.values().next().unwrap().view.entity_id()
    });

    text.update(cx, |text, cx| {
        text.set_text(&format!("{source}```\n\nAfter"), cx)
    });

    cx.run_until_parked();
    cx.refresh().unwrap();

    cache.read_with(cx, |cache, _| {
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(
            cache.entries.values().next().unwrap().view.entity_id(),
            original
        );
    });
}

#[gpui::test]
fn diagram_cache_reuses_sources_replaces_edits_and_evicts_old_entries(cx: &mut TestAppContext) {
    let owner = cx.new(|_| TranscriptView::new(AgentKind::Codex, None));
    let cache = cx.new(|_| MermaidCache::new(owner.downgrade()));

    let first = DiagramKey {
        entry: 0,
        part: TextPart::Reply,
        offset: 0,
    };

    cache.update(cx, |cache, cx| {
        cache.ensure(first, "flowchart TD\nA --> B", cx);

        let original = cache.entries[&first].view.entity_id();

        cache.ensure(first, "flowchart TD\nA --> B", cx);

        assert_eq!(cache.entries[&first].view.entity_id(), original);

        cache.ensure(first, "flowchart TD\nA --> C", cx);

        assert_ne!(cache.entries[&first].view.entity_id(), original);

        for entry in 1..=MAX_ENTRIES {
            cache.ensure(DiagramKey { entry, ..first }, "flowchart LR\nA --> B", cx);
        }

        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert!(!cache.entries.contains_key(&first));
    });
}

#[gpui::test]
fn visible_diagrams_survive_cache_eviction_without_restarting(cx: &mut TestAppContext) {
    let owner = cx.new(|_| TranscriptView::new(AgentKind::Codex, None));
    let cache = cx.new(|_| MermaidCache::new(owner.downgrade()));

    cache.update(cx, |cache, cx| {
        let key = DiagramKey {
            entry: 0,
            part: TextPart::Reply,
            offset: 0,
        };

        let source = "flowchart TD\nA --> B";

        let visible = (0..MAX_ENTRIES + 4)
            .map(|offset| cache.ensure(DiagramKey { offset, ..key }, source, cx))
            .collect::<Vec<_>>();

        assert_eq!(cache.entries.len(), MAX_ENTRIES);

        for (offset, original) in visible.iter().enumerate() {
            let reused = cache.ensure(DiagramKey { offset, ..key }, source, cx);

            assert_eq!(reused.entity_id(), original.entity_id());
        }
    });
}
