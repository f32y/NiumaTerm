#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use futures::channel::oneshot;
use gpui::{
    App, AppContext as _, Bounds, Context, Entity, FollowMode, ListOffset, Pixels, Task, Window,
    fill, point, px, rgba, size,
};
use gpui_base::text::search::LiteralSearch;
use nmt_terminal::event::{Msg, MsgSender, Query};
use nmt_terminal::ghostty::{CellWide, ScreenRowRead};
use nmt_terminal::session::page::{PAGE_ROWS, PageSource};

use crate::find::{FindBar, FindEvent, adjacent};
use crate::terminal_tab::block_list::FrozenView;
use crate::terminal_tab::frame::TerminalFrame;
use crate::terminal_tab::metrics::CellMetrics;
use crate::terminal_tab::view::TerminalPane;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Hit {
    source: PageSource,
    start: (usize, u16),
    end: (usize, u16),
}

impl Hit {
    fn same_position(&self, other: &Self) -> bool {
        same_source(self.source, other.source) && self.start == other.start && self.end == other.end
    }

    fn columns(&self, row: usize, cols: u16) -> Option<Range<u16>> {
        if row < self.start.0 || row > self.end.0 {
            return None;
        }

        Some(
            if row == self.start.0 { self.start.1 } else { 0 }..if row == self.end.0 {
                self.end.1
            } else {
                cols
            },
        )
    }
}

fn same_source(a: PageSource, b: PageSource) -> bool {
    match (a, b) {
        (PageSource::Screen { .. }, PageSource::Screen { .. }) => true,
        (PageSource::Block { id: a, .. }, PageSource::Block { id: b, .. }) => a == b,
        _ => false,
    }
}

#[derive(Default)]
pub(super) struct SearchState {
    pub(super) bar: Option<Entity<FindBar>>,
    query: String,
    task: Option<Task<()>>,
    sources: Vec<(PageSource, usize)>,
    cache: HashMap<PageSource, Arc<Vec<Hit>>>,
    hits: Vec<Hit>,
    current: Option<usize>,
    initial: bool,
}

pub(crate) struct RowHighlight {
    y: f32,
    columns: Vec<(Range<u16>, bool)>,
}

pub(super) fn frame_highlights(
    pane: &TerminalPane,
    frame: &TerminalFrame,
    cell: CellMetrics,
    slack: f32,
    cx: &App,
) -> Vec<RowHighlight> {
    let source = PageSource::Screen {
        revision: pane.model.source.snapshot.revision(),
    };

    frame
        .lines()
        .iter()
        .enumerate()
        .filter_map(|(index, _)| {
            let columns = pane.search.row_highlights(
                source,
                frame.scrollbar().offset as usize + index,
                pane.model.source.snapshot.cols() as u16,
                cx,
            );

            (!columns.is_empty()).then_some(RowHighlight {
                y: index as f32 * cell.height_px + slack,
                columns,
            })
        })
        .collect()
}

pub(super) fn frozen_highlights(
    pane: &TerminalPane,
    view: &FrozenView,
    cx: &App,
) -> Vec<RowHighlight> {
    view.rows
        .iter()
        .filter_map(|row| {
            let source = if row.item == usize::MAX {
                PageSource::Screen {
                    revision: pane.model.source.snapshot.revision(),
                }
            } else {
                let handle = pane.model.source.session.block_item(row.item)?.handle()?;

                PageSource::Block {
                    id: handle.id,
                    generation: handle.generation,
                    theme: pane.model.source.snapshot.theme_revision(),
                }
            };

            let columns = pane
                .search
                .row_highlights(source, row.row, row.cell_count as u16, cx);

            (!columns.is_empty()).then_some(RowHighlight { y: row.y, columns })
        })
        .collect()
}

pub(crate) fn paint_highlights(
    bounds: Bounds<Pixels>,
    cell: CellMetrics,
    rows: &[RowHighlight],
    window: &mut Window,
) {
    for row in rows {
        paint_row(bounds, row.y, cell, &row.columns, window);
    }
}

impl SearchState {
    pub(super) fn visible(&self, cx: &App) -> bool {
        self.bar.as_ref().is_some_and(|bar| bar.read(cx).visible)
    }

    pub(super) fn row_highlights(
        &self,
        source: PageSource,
        row: usize,
        cols: u16,
        cx: &App,
    ) -> Vec<(Range<u16>, bool)> {
        if !self.visible(cx) {
            return Vec::new();
        }

        self.hits
            .iter()
            .enumerate()
            .filter_map(|(index, hit)| {
                // A resized block uses different physical rows, so old geometry is not drawn.
                let valid = match (hit.source, source) {
                    (
                        PageSource::Block {
                            id: a,
                            generation: ag,
                            ..
                        },
                        PageSource::Block {
                            id: b,
                            generation: bg,
                            ..
                        },
                    ) => a == b && ag == bg,
                    (PageSource::Screen { revision: a }, PageSource::Screen { revision: b }) => {
                        a == b
                    }
                    _ => false,
                };

                valid
                    .then(|| hit.columns(row, cols))
                    .flatten()
                    .map(|range| (range, self.current == Some(index)))
            })
            .collect()
    }
}

pub(super) fn open(pane: &mut TerminalPane, window: &mut Window, cx: &mut Context<TerminalPane>) {
    if pane.search.bar.is_none() {
        let bar = cx.new(|cx| FindBar::new(window, cx));

        cx.subscribe(&bar, |pane, _, event, cx| handle_event(pane, event, cx))
            .detach();

        pane.search.bar = Some(bar);
    }

    pane.search
        .bar
        .clone()
        .expect("search bar")
        .update(cx, |bar, cx| bar.open(window, cx));

    cx.notify();
}

fn handle_event(pane: &mut TerminalPane, event: &FindEvent, cx: &mut Context<TerminalPane>) {
    match event {
        FindEvent::Editing => {
            pane.search.task = None;

            pane.search.query.clear();
            pane.search.hits.clear();

            pane.search.current = None;

            cx.notify();

            return;
        }
        FindEvent::Query(query) => {
            if *query == pane.search.query && !pane.search.hits.is_empty() {
                reveal(pane, cx);
                refresh(pane, cx);
                update_bar(pane, cx);

                return;
            }

            pane.search.task = None;
            pane.search.query = query.clone();

            pane.search.sources.clear();
            pane.search.cache.clear();
            pane.search.hits.clear();

            pane.search.current = None;
            pane.search.initial = true;

            refresh(pane, cx);
        }
        FindEvent::Navigate { backwards } => {
            pane.search.current = adjacent(pane.search.current, pane.search.hits.len(), *backwards);

            reveal(pane, cx);
        }
        FindEvent::Close => {
            pane.search.task = None;

            pane.search.sources.clear();
        }
    }

    update_bar(pane, cx);

    cx.notify();
}

fn update_bar(pane: &TerminalPane, cx: &mut Context<TerminalPane>) {
    if let Some(bar) = &pane.search.bar {
        bar.update(cx, |bar, cx| {
            bar.results(
                pane.search.current,
                pane.search.hits.len(),
                pane.search.initial && pane.search.task.is_some(),
                cx,
            )
        });
    }
}

pub(super) fn refresh(pane: &mut TerminalPane, cx: &mut Context<TerminalPane>) {
    if !pane.search.visible(cx) || pane.search.query.is_empty() || pane.search.task.is_some() {
        return;
    }

    let sources = page_sources(pane);

    if pane.search.sources == sources {
        return;
    }

    pane.search.sources = sources.clone();

    pane.search
        .cache
        .retain(|key, _| sources.iter().any(|(source, _)| key == source));

    let cache = pane.search.cache.clone();
    let sender = pane.model.source.session.messenger();
    let query = LiteralSearch::new(&pane.search.query);

    pane.search.task = Some(cx.spawn(async move |this, cx| {
        // Coalesce output notifications without delaying the input event handler.
        cx.background_executor()
            .timer(Duration::from_millis(100))
            .await;

        // Output during the delay can replace the screen revision before any page is read.
        let Ok(sources) = this.update(cx, |pane, _| {
            let sources = page_sources(pane);

            pane.search.sources = sources.clone();

            sources
        }) else {
            return;
        };

        let result = cx
            .background_executor()
            .spawn(async move {
                let mut completed = HashMap::new();
                let mut hits = Vec::new();
                let mut all_ready = true;

                for (source, rows) in sources {
                    let found = if let Some(cached) = cache.get(&source) {
                        Some(cached.clone())
                    } else {
                        scan_source(&sender, source, rows, &query)
                            .await
                            .map(Arc::new)
                    };

                    if let Some(found) = found {
                        hits.extend(found.iter().cloned());
                        completed.insert(source, found);
                    } else {
                        all_ready = false;
                    }
                }

                (hits, completed, all_ready)
            })
            .await;

        let _ = this.update(cx, |pane, cx| {
            let (hits, cache, all_ready) = result;

            let previous = pane
                .search
                .current
                .and_then(|index| pane.search.hits.get(index))
                .cloned();

            pane.search.cache = cache;
            pane.search.task = None;

            if all_ready {
                let current = previous
                    .as_ref()
                    .and_then(|old| hits.iter().position(|hit| hit.same_position(old)));

                let fallback = pane
                    .search
                    .current
                    .unwrap_or(0)
                    .min(hits.len().saturating_sub(1));

                pane.search.current = current.or_else(|| (!hits.is_empty()).then_some(fallback));
                pane.search.hits = hits;

                if pane.search.initial {
                    choose_near_viewport(pane);
                    reveal(pane, cx);

                    pane.search.initial = false;
                }
            } else {
                pane.search.sources.clear();
            }

            update_bar(pane, cx);

            cx.notify();
        });
    }));

    update_bar(pane, cx);
}

fn page_sources(pane: &TerminalPane) -> Vec<(PageSource, usize)> {
    let session = &pane.model.source.session;
    let snapshot = session.snapshot();

    let mut sources = Vec::new();

    if !session.alt_screen() {
        for item in session.block_store().lock().items() {
            if let Some(handle) = item.handle() {
                sources.push((
                    PageSource::Block {
                        id: handle.id,
                        generation: handle.generation,
                        theme: snapshot.theme_revision(),
                    },
                    item.engine_rows(),
                ));
            }
        }
    }

    sources.push((
        PageSource::Screen {
            revision: snapshot.revision(),
        },
        snapshot.scrollbar().total as usize,
    ));

    sources
}

fn choose_near_viewport(pane: &mut TerminalPane) {
    let offset = pane.block_list.list.logical_scroll_top();
    let store = pane.model.source.session.block_store();
    let store = store.lock();
    let cell_h = pane.model.cell_metrics.map_or(1., |cell| cell.height_px);
    let row = (offset.offset_in_item.as_f32() / cell_h - pane.model.pad_rows).max(0.) as usize;
    let screen_row = pane.model.source.session.snapshot().scrollbar().offset as usize;

    pane.search.current = pane
        .search
        .hits
        .iter()
        .position(|hit| {
            if pane.model.block_list_mode() {
                let item = match hit.source {
                    PageSource::Block { id, .. } => store
                        .items()
                        .iter()
                        .position(|item| item.handle().is_some_and(|h| h.id == id))
                        .unwrap_or(0),
                    PageSource::Screen { .. } => store.items().len(),
                };

                (item, hit.start.0) >= (offset.item_ix, row)
            } else {
                hit.start.0 >= screen_row
            }
        })
        .or_else(|| (!pane.search.hits.is_empty()).then_some(0));
}

fn reveal(pane: &mut TerminalPane, cx: &mut Context<TerminalPane>) {
    let Some(hit) = pane
        .search
        .current
        .and_then(|index| pane.search.hits.get(index))
    else {
        return;
    };

    if pane.model.block_list_mode() {
        let store = pane.model.source.session.block_store();
        let store = store.lock();

        let item = match hit.source {
            PageSource::Block { id, .. } => store
                .items()
                .iter()
                .position(|item| item.handle().is_some_and(|h| h.id == id)),
            PageSource::Screen { .. } => Some(store.items().len()),
        };

        if let Some(item_ix) = item {
            let height = pane.model.cell_metrics.map_or(16., |cell| cell.height_px);

            pane.block_list.list.set_follow_mode(FollowMode::Normal);

            pane.block_list.list.scroll_to(ListOffset {
                item_ix,
                offset_in_item: px(
                    ((hit.start.0 as f32 + pane.model.pad_rows) * height - 48.).max(0.)
                ),
            });
        }
    } else {
        pane.model
            .source
            .session
            .scroll_to(hit.start.0.saturating_sub(3) as u64);
    }

    update_bar(pane, cx);

    cx.notify();
}

struct CellSpan {
    bytes: Range<usize>,
    row: usize,
    col: u16,
    end: u16,
}

#[derive(Default)]
struct LogicalLine {
    text: String,
    cells: Vec<CellSpan>,
}

impl LogicalLine {
    fn push(&mut self, row: &ScreenRowRead, index: usize, cols: u16) {
        let mut col = 0;

        for cell in &row.cells {
            while col < cell.x {
                self.cell(" ", index, col, col + 1);

                col += 1;
            }

            match cell.wide {
                CellWide::SpacerTail | CellWide::SpacerHead => {
                    col = col.max(cell.x + 1);
                }
                CellWide::Narrow | CellWide::Wide => {
                    let end =
                        cell.x
                            .saturating_add(if cell.wide == CellWide::Wide { 2 } else { 1 });

                    self.cell(
                        if cell.text.is_empty() {
                            " "
                        } else {
                            &cell.text
                        },
                        index,
                        cell.x,
                        end,
                    );

                    col = end;
                }
            }
        }

        if row.meta.wrapped {
            while col < cols {
                self.cell(" ", index, col, col + 1);

                col += 1;
            }
        }
    }

    fn cell(&mut self, text: &str, row: usize, col: u16, end: u16) {
        let start = self.text.len();

        self.text.push_str(text);

        self.cells.push(CellSpan {
            bytes: start..self.text.len(),
            row,
            col,
            end,
        });
    }

    fn finish(&mut self, source: PageSource, query: &LiteralSearch, hits: &mut Vec<Hit>) {
        for range in query.ranges(&self.text) {
            let first = self
                .cells
                .partition_point(|cell| cell.bytes.end <= range.start);

            let last = self
                .cells
                .partition_point(|cell| cell.bytes.start < range.end)
                .saturating_sub(1);

            if let (Some(first), Some(last)) = (self.cells.get(first), self.cells.get(last)) {
                hits.push(Hit {
                    source,
                    start: (first.row, first.col),
                    end: (last.row, last.end),
                });
            }
        }

        self.text.clear();
        self.cells.clear();
    }
}

async fn scan_source(
    sender: &MsgSender,
    source: PageSource,
    rows: usize,
    query: &LiteralSearch,
) -> Option<Vec<Hit>> {
    let mut hits = Vec::new();
    let mut line = LogicalLine::default();

    for start in (0..rows).step_by(PAGE_ROWS) {
        let (reply, request) = oneshot::channel();

        sender
            .send(Msg::Query(Query::Rows {
                source,
                start,
                reply,
            }))
            .ok()?;

        let page = request.await.ok()?.ok()?;

        for (offset, row) in page.rows.iter().enumerate() {
            line.push(row, start + offset, page.cols);

            if !row.meta.wrapped {
                line.finish(source, query, &mut hits);
            }
        }
    }

    line.finish(source, query, &mut hits);

    Some(hits)
}

pub(super) fn paint_row(
    bounds: Bounds<Pixels>,
    y: f32,
    cell: CellMetrics,
    highlights: &[(Range<u16>, bool)],
    window: &mut Window,
) {
    for (range, active) in highlights {
        window.paint_quad(fill(
            Bounds::new(
                point(
                    bounds.left() + px(range.start as f32 * cell.width_px),
                    bounds.top() + px(y),
                ),
                size(px(range.len() as f32 * cell.width_px), px(cell.height_px)),
            ),
            if *active {
                rgba(0xe8a52be0)
            } else {
                rgba(0xe8c14c70)
            },
        ));
    }
}
