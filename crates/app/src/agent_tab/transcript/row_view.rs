use std::cell::Cell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, Pixels, Render, StyleRefinement, TextStyle, WeakEntity, Window,
    div,
};
use gpui_base::ElementExt as _;
use nmt_agent::transcript::conversation::{ContentChange, EntryChange};

use crate::agent_tab::transcript::rows::{RowSpec, TranscriptRow};
use crate::agent_tab::transcript::view::TranscriptView;

const RETAINED_ROWS: usize = 256;

#[derive(Default)]
pub(super) struct RowViews {
    rows: HashMap<usize, RetainedRow>,
    clock: u64,
    style: Option<(TextStyle, Pixels, f32)>,
}

struct RetainedRow {
    view: Entity<RowView>,
    signature: TranscriptRow,
    height: Rc<Cell<Option<Pixels>>>,
    used: u64,
    moving: bool,
}

struct RowView {
    owner: WeakEntity<TranscriptView>,
    index: usize,
    height: Rc<Cell<Option<Pixels>>>,
}

impl RowViews {
    pub(super) fn clear(&mut self) {
        self.rows.clear();
    }

    pub(super) fn invalidate(&mut self, range: Range<usize>) {
        for (&index, row) in &self.rows {
            if range.contains(&index) {
                row.height.set(None);
            }
        }
    }

    pub(super) fn content_changed(&mut self, change: &ContentChange) {
        for row in self.rows.values() {
            let (RowSpec::Entry { index, .. } | RowSpec::Work { index, .. }) = row.signature.spec
            else {
                continue;
            };

            let changed = change.reset_from.is_some_and(|first| first <= index)
                || (change.entries.is_empty() && index >= change.first)
                || change.entries.iter().any(|&(entry, update)| {
                    entry == index
                        && (update == EntryChange::Replace || row.signature.block.is_none())
                });

            if changed {
                row.height.set(None);
            }
        }
    }

    pub(super) fn render(
        &mut self,
        index: usize,
        signature: &TranscriptRow,
        moving: bool,
        window: &mut Window,
        cx: &mut Context<TranscriptView>,
    ) -> AnyElement {
        let style = (
            window.text_style(),
            window.rem_size(),
            window.scale_factor(),
        );

        if self.style.as_ref() != Some(&style) {
            self.invalidate(0..usize::MAX);

            self.style = Some(style);
        }

        self.clock += 1;

        if self.rows.len() >= RETAINED_ROWS
            && !self.rows.contains_key(&index)
            && let Some(oldest) = self
                .rows
                .iter()
                .min_by_key(|(_, row)| row.used)
                .map(|(&i, _)| i)
        {
            self.rows.remove(&oldest);
        }

        let row = self.rows.entry(index).or_insert_with(|| {
            let height = Rc::new(Cell::new(None));
            let owner = cx.entity().downgrade();

            let view = cx.new(|_| RowView {
                owner,
                index,
                height: height.clone(),
            });

            RetainedRow {
                view,
                signature: signature.clone(),
                height,
                used: 0,
                moving: false,
            }
        });

        row.used = self.clock;

        if row.signature != *signature
            || moving
            || row.moving
            || window.is_view_dirty(row.view.entity_id())
        {
            row.signature = signature.clone();

            row.height.set(None);
        }

        row.moving = moving;

        // GPUI requires a definite size for scene reuse. New content and dirty
        // descendants first measure normally; the next draw can reuse that size.
        // Keep the list's clip bounds: text selection uses the content mask to
        // detect viewport edges. A per-row clip would start auto-scroll while
        // selecting text in the middle of the list.
        match row.height.get() {
            Some(height) => row
                .view
                .clone()
                .cached(StyleRefinement::default().w_full().h(height))
                .into_any_element(),
            None => row.view.clone().into_any_element(),
        }
    }
}

impl Render for RowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self
            .owner
            .update(cx, |view, cx| view.render_row(self.index, window, cx))
            .unwrap_or_else(|_| div().into_any_element());

        let height = self.height.clone();

        div()
            .debug_selector(|| format!("transcript-row-{}", self.index))
            .w_full()
            .child(body)
            .on_prepaint(move |bounds, _, _| {
                height.set(Some(bounds.size.height));
            })
    }
}
