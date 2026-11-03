#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    App, AppContext as _, Bounds, Context, Entity, FollowMode, ListOffset, Pixels, Task, Window,
    point, px, rgba,
};
use gpui_base::text::search::{
    LiteralSearch, TextSearch, TextSearchMatch, markdown_matches, plain_matches,
};
use nmt_agent::chat::Item;
use parking_lot::Mutex;

use crate::agent_tab::composer::parse_annotated_prompt;
use crate::agent_tab::transcript::code::{is_code_item, search_text};
use crate::agent_tab::transcript::render::work_card::work_step;
use crate::agent_tab::transcript::reveal::RevealKey;
use crate::agent_tab::transcript::text::TextPart;
use crate::agent_tab::transcript::{RowSpec, TranscriptView, is_work_row};
use crate::find::{FindBar, FindEvent, adjacent};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum SearchPart {
    Text(TextPart),
    Code,
}

struct Section {
    entry: usize,
    part: SearchPart,
    matches: Arc<Vec<TextSearchMatch>>,
}

#[derive(Clone, PartialEq, Eq)]
struct Location {
    entry: usize,
    part: SearchPart,
    found: TextSearchMatch,
    occurrence: usize,
}

impl Location {
    fn same_position(&self, other: &Self) -> bool {
        self.entry == other.entry
            && self.part == other.part
            && self.found.source.start == other.found.source.start
            && self.found.range == other.found.range
    }
}

#[derive(Default)]
pub(super) struct SearchState {
    pub(super) bar: Option<Entity<FindBar>>,
    query: String,
    version: Option<(u64, u64)>,
    sections: Vec<Section>,
    hits: Vec<Location>,
    current: Option<usize>,
    task: Option<Task<()>>,
    initial: bool,
    pub(super) automatic: HashSet<RevealKey>,
    pub(super) pending: bool,
    bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
}

impl SearchState {
    pub(super) fn visible(&self, cx: &App) -> bool {
        self.bar.as_ref().is_some_and(|bar| bar.read(cx).visible)
    }
}

pub(super) fn open(
    view: &mut TranscriptView,
    window: &mut Window,
    cx: &mut Context<TranscriptView>,
) {
    if view.search.bar.is_none() {
        let bar = cx.new(|cx| FindBar::new(window, cx));

        cx.subscribe(&bar, |view, _, event, cx| handle_event(view, event, cx))
            .detach();

        view.search.bar = Some(bar);
    }

    view.search
        .bar
        .clone()
        .expect("search bar")
        .update(cx, |bar, cx| bar.open(window, cx));

    cx.notify();
}

fn handle_event(view: &mut TranscriptView, event: &FindEvent, cx: &mut Context<TranscriptView>) {
    match event {
        FindEvent::Editing => {
            view.search.task = None;
            view.search.pending = false;

            view.search.query.clear();
            view.search.sections.clear();
            view.search.hits.clear();

            view.search.current = None;

            decorate(view, cx);

            cx.notify();

            return;
        }
        FindEvent::Query(query) => {
            if *query == view.search.query && !view.search.hits.is_empty() {
                reveal(view, cx);
                refresh(view, cx);
                update_bar(view, cx);

                return;
            }

            view.search.task = None;
            view.search.query = query.clone();
            view.search.version = None;

            view.search.sections.clear();
            view.search.hits.clear();

            view.search.current = None;
            view.search.initial = true;

            if query.is_empty() {
                restore(view);
            }

            decorate(view, cx);
            refresh(view, cx);
        }
        FindEvent::Navigate { backwards } => {
            view.search.current = adjacent(view.search.current, view.search.hits.len(), *backwards);

            reveal(view, cx);
        }
        FindEvent::Close => {
            view.search.task = None;
            view.search.pending = false;

            restore(view);

            view.text_views.search.clear();
            view.code_transcripts.set_search(HashMap::new(), cx);
        }
    }

    update_bar(view, cx);

    cx.notify();
}

fn restore(view: &mut TranscriptView) {
    for key in view.search.automatic.drain().collect::<Vec<_>>() {
        view.take_down_disclosure(key);
    }
}

pub(super) fn reset(view: &mut TranscriptView) {
    view.search.task = None;
    view.search.version = None;

    view.search.sections.clear();
    view.search.hits.clear();

    view.search.current = None;
    view.search.pending = false;
    view.search.initial = true;

    view.search.automatic.clear();
}

fn update_bar(view: &TranscriptView, cx: &mut Context<TranscriptView>) {
    if let Some(bar) = &view.search.bar {
        bar.update(cx, |bar, cx| {
            bar.results(
                view.search.current,
                view.search.hits.len(),
                view.search.initial && view.search.task.is_some(),
                cx,
            )
        });
    }
}

pub(super) fn refresh(view: &mut TranscriptView, cx: &mut Context<TranscriptView>) {
    if !view.search.visible(cx) || view.search.query.is_empty() || view.search.task.is_some() {
        return;
    }

    let conversation = view.conversation.borrow();
    let version = conversation.version();

    if view.search.version == Some(version) {
        return;
    }

    let first = view
        .search
        .version
        .and_then(|old| conversation.changes_since(old))
        .map_or(0, |change| change.first);

    let items: Vec<_> = conversation
        .content
        .entries()
        .iter()
        .enumerate()
        .skip(first)
        .map(|(index, entry)| (index, entry.item.clone()))
        .collect();

    drop(conversation);

    let query = LiteralSearch::new(&view.search.query);

    view.search.task = Some(cx.spawn(async move |this, cx| {
        let sections = cx
            .background_executor()
            .spawn(async move {
                let mut sections = Vec::new();

                for (entry, item) in items {
                    collect_sections(entry, &item, &query, &mut sections);
                }

                sections
            })
            .await;

        let _ = this.update(cx, |view, cx| {
            view.search.task = None;

            if view.conversation.borrow().version().0 != version.0 {
                view.search.version = None;

                view.search.sections.clear();
                view.search.hits.clear();

                view.search.current = None;

                view.search.automatic.clear();

                decorate(view, cx);

                cx.notify();

                return;
            }

            let previous = view
                .search
                .current
                .and_then(|index| view.search.hits.get(index))
                .cloned();

            view.search.sections.retain(|section| section.entry < first);
            view.search.sections.extend(sections);

            view.search.version = Some(version);

            view.search.hits = view
                .search
                .sections
                .iter()
                .flat_map(|section| {
                    section
                        .matches
                        .iter()
                        .enumerate()
                        .map(|(occurrence, found)| Location {
                            entry: section.entry,
                            part: section.part,
                            found: found.clone(),
                            occurrence,
                        })
                })
                .collect();

            let current = previous.as_ref().and_then(|old| {
                view.search
                    .hits
                    .iter()
                    .position(|hit| hit.same_position(old))
            });

            let fallback = view
                .search
                .current
                .unwrap_or(0)
                .min(view.search.hits.len().saturating_sub(1));

            view.search.current =
                current.or_else(|| (!view.search.hits.is_empty()).then_some(fallback));

            if view.search.initial {
                let top = view.transcript_list.logical_scroll_top().item_ix;

                let entry = view
                    .rows
                    .get(top)
                    .and_then(|row| match row.spec {
                        RowSpec::Entry { index, .. } | RowSpec::Work { index, .. } => Some(index),
                        _ => None,
                    })
                    .unwrap_or(0);

                view.search.current = view
                    .search
                    .hits
                    .iter()
                    .position(|hit| hit.entry >= entry)
                    .or_else(|| (!view.search.hits.is_empty()).then_some(0));

                view.search.initial = false;

                reveal(view, cx);
            } else {
                decorate(view, cx);
            }

            update_bar(view, cx);

            cx.notify();
        });
    }));

    update_bar(view, cx);
}

fn collect_sections(entry: usize, item: &Item, query: &LiteralSearch, sections: &mut Vec<Section>) {
    let mut text = |part, source: &str, markdown| {
        let matches = if markdown {
            markdown_matches(source, query).unwrap_or_default()
        } else {
            plain_matches(source, query)
        };

        if !matches.is_empty() {
            sections.push(Section {
                entry,
                part,
                matches: Arc::new(matches),
            });
        }
    };

    match item {
        Item::UserMessage { text: Some(source) } => {
            let parsed = parse_annotated_prompt(source);

            text(
                SearchPart::Text(TextPart::Prompt),
                parsed
                    .as_ref()
                    .map_or(source.as_str(), |parsed| parsed.prompt),
                false,
            );

            if let Some(parsed) = parsed {
                for (index, annotation) in parsed.annotations.iter().enumerate() {
                    text(
                        SearchPart::Text(TextPart::Annotation(index)),
                        &annotation.text,
                        false,
                    );
                }
            }
        }
        Item::AgentMessage {
            text: Some(source), ..
        } => text(SearchPart::Text(TextPart::Reply), source, true),
        Item::Error { text: source } => text(SearchPart::Text(TextPart::Error), source, false),
        Item::Compaction { detail, .. } => {
            if let Some(summary) = &detail.summary {
                text(SearchPart::Text(TextPart::Summary), summary, true);
            }
        }
        Item::SideBoundary {
            instructions,
            message,
            ..
        } => {
            text(SearchPart::Text(TextPart::Instructions), instructions, true);
            text(SearchPart::Text(TextPart::Message), message, true);
        }
        item if is_code_item(item) => {
            if let Some(source) = search_text(item) {
                text(SearchPart::Code, &source, false);
            }
        }
        item => {
            if let Some(detail) = work_step(item).and_then(|step| step.detail) {
                text(SearchPart::Text(TextPart::Work), detail, true);
            }
        }
    }
}

fn decorate(view: &mut TranscriptView, cx: &mut Context<TranscriptView>) {
    let active = view
        .search
        .current
        .and_then(|index| view.search.hits.get(index));

    let mut code = HashMap::new();

    view.text_views.search.clear();

    for section in &view.search.sections {
        let search = TextSearch {
            matches: section.matches.clone(),
            active: active
                .filter(|hit| hit.entry == section.entry && hit.part == section.part)
                .map(|hit| hit.occurrence),
            background: rgba(0xe8c14c70).into(),
            active_background: rgba(0xe8a52be0).into(),
            active_bounds: view.search.bounds.clone(),
        };

        match section.part {
            SearchPart::Text(part) => {
                view.text_views.search.insert((section.entry, part), search);
            }
            SearchPart::Code => {
                code.insert(section.entry, search);
            }
        }
    }

    view.code_transcripts.set_search(code, cx);
}

fn reveal(view: &mut TranscriptView, cx: &mut Context<TranscriptView>) {
    let Some(hit) = view
        .search
        .current
        .and_then(|index| view.search.hits.get(index))
        .cloned()
    else {
        decorate(view, cx);

        return;
    };

    view.finish_typing();
    view.transcript_list.set_follow_mode(FollowMode::Normal);

    let shared = view.conversation.clone();
    let conversation = shared.borrow();
    let items = conversation.content.entries();

    let Some(entry) = items.get(hit.entry) else {
        return;
    };

    let turn = entry.turn;

    let mut keys = vec![RevealKey::Row(hit.entry)];

    if matches!(hit.part, SearchPart::Text(TextPart::Annotation(_))) {
        keys.push(RevealKey::Annotation(hit.entry));
    }

    if !view.rows.iter().any(|row| matches!(row.spec, RowSpec::Entry { index, .. } | RowSpec::Work { index, .. } if index == hit.entry)) {
        keys.push(RevealKey::Turn(turn));
    }

    if is_work_row(&entry.item) {
        let mut first = hit.entry;

        while first > 0 && items[first - 1].turn == turn && is_work_row(&items[first - 1].item) {
            first -= 1;
        }

        keys.push(RevealKey::Group(first));
    }

    drop(conversation);

    for key in keys {
        if !view.disclosures.is_disclosing(key) {
            view.disclosures.open(key, Instant::now(), false);
            view.search.automatic.insert(key);
        }
    }

    view.row_cache.invalidate(hit.entry);
    view.measurements.invalidate(hit.entry);

    view.search.pending = true;
    *view.search.bounds.lock() = None;

    decorate(view, cx);

    cx.notify();
}

pub(super) fn after_rows(
    view: &mut TranscriptView,
    window: &mut Window,
    cx: &mut Context<TranscriptView>,
) {
    if !view.search.pending {
        return;
    }

    let Some(hit) = view
        .search
        .current
        .and_then(|index| view.search.hits.get(index))
        .cloned()
    else {
        return;
    };

    let row = view.rows.iter().position(|row| {
        matches!(row.spec, RowSpec::Entry { index, .. } | RowSpec::Work { index, .. } if index == hit.entry)
            && row.block.as_ref().is_none_or(|block| hit.found.source.start >= block.key.0 && hit.found.source.start < block.key.1)
    });

    let Some(row) = row else {
        return;
    };

    if view.search.bounds.lock().is_none() {
        view.transcript_list.scroll_to(ListOffset {
            item_ix: row,
            offset_in_item: px(0.),
        });
    }

    let this = cx.entity().downgrade();

    window.on_next_frame(move |_, cx| {
        let _ = this.update(cx, finish_reveal);
    });
}

fn finish_reveal(view: &mut TranscriptView, cx: &mut Context<TranscriptView>) {
    if !view.search.pending {
        return;
    }

    let Some(bounds) = *view.search.bounds.lock() else {
        return;
    };

    let Some(hit) = view
        .search
        .current
        .and_then(|index| view.search.hits.get(index))
    else {
        return;
    };

    if let SearchPart::Text(part) = hit.part
        && let Some(scroll) = view.text_views.search_scrolls.get(&(hit.entry, part))
    {
        let viewport = scroll.bounds();

        let delta = if bounds.top() < viewport.top() {
            bounds.top() - viewport.top()
        } else if bounds.bottom() > viewport.bottom() {
            bounds.bottom() - viewport.bottom()
        } else {
            px(0.)
        };

        if delta.abs() > px(1.) {
            let old = scroll.offset();

            scroll.set_offset(point(old.x, old.y - delta));

            if scroll.offset() != old {
                *view.search.bounds.lock() = None;

                cx.notify();

                return;
            }
        }
    }

    let viewport = view.transcript_list.viewport_bounds();
    let top = viewport.top() + px(48.);
    let bottom = viewport.bottom() - px(16.);

    let delta = if bounds.top() < top {
        bounds.top() - top
    } else if bounds.bottom() > bottom {
        bounds.bottom() - bottom
    } else {
        px(0.)
    };

    view.transcript_list.freeze_scroll_position();
    view.transcript_list.scroll_by(delta);

    view.search.pending = false;

    cx.notify();
}
