use std::{panic, sync};

use gpui::{
    App, Bounds, Element, ElementId, Entity, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, ShapedLine, Style, Window, fill, point, px, relative, rgb, size,
};
use nmt_terminal::block_store::BlockStore;
use parking_lot::Mutex;

use crate::terminal_tab::block_list::live::LiveItemState;
use crate::terminal_tab::block_list::{FrozenRow, FrozenView};
use crate::terminal_tab::frame::{TerminalColor, TerminalFrame};
use crate::terminal_tab::frame_source::ItemViewport;
use crate::terminal_tab::layout::frame_content_rows;
use crate::terminal_tab::metrics::CellMetrics;
use crate::terminal_tab::paint::frame::{paint_frame, paint_frozen_images, shape_frame};
use crate::terminal_tab::paint::text::{paint_glyph_rows, paint_line_backgrounds_at, shape_lines};
use crate::terminal_tab::pane_model::frame_record::FrameRecord;
use crate::terminal_tab::view::TerminalPane;
use crate::terminal_tab::{block_list, metrics};

type SharedBlockStore = sync::Arc<Mutex<BlockStore>>;

pub(crate) enum BlockListItem {
    Frozen {
        item_idx: usize,
        store: SharedBlockStore,
        cols: u32,
        cell: metrics::CellMetrics,
        selection: Option<(block_list::FrozenPoint, block_list::FrozenPoint)>,
        pane: Entity<TerminalPane>,
    },
    Live {
        frame: TerminalFrame,

        /// Active-grid scrollback rows rendered above the live grid
        /// when scrolling into a running command.
        history_rows: u64,

        state: LiveItemState,
        cols: u32,
        cell: metrics::CellMetrics,
        pane: Entity<TerminalPane>,
    },
}

impl IntoElement for BlockListItem {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

pub(crate) enum BlockListItemPrepaint {
    Frozen {
        view: block_list::FrozenView,
        shaped: Vec<ShapedLine>,
    },
    Live {
        tail_view: block_list::FrozenView,
        tail_shaped: Vec<ShapedLine>,
        active_shaped: Vec<ShapedLine>,
        active_bounds: Bounds<Pixels>,
    },
}

impl Element for BlockListItem {
    type RequestLayoutState = Style;

    type PrepaintState = BlockListItemPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Style) {
        let mut style = Style::default();

        style.size.width = relative(1.0).into();

        let pad_rows = self.pane().read(cx).model.pad_rows;

        let height = match self {
            BlockListItem::Frozen {
                item_idx,
                store,
                cols,
                cell,
                ..
            } => {
                let store = store.lock();

                store
                    .items()
                    .get(*item_idx)
                    .map(|item| block_list::item_px(item, *cols, cell.height_px, pad_rows))
                    .unwrap_or(0.0)
            }
            BlockListItem::Live {
                frame,
                history_rows,
                cell,
                ..
            } => block_list::live_item_px(
                *history_rows,
                frame_content_rows(frame),
                cell.height_px,
                pad_rows,
            ),
        }
        .max(0.0);

        style.size.height = px(height).into();

        let layout_id = window.request_layout(style.clone(), [], cx);

        (layout_id, style)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Style,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let theme = self.pane().read(cx).model.theme;
        let origin_y = self.pane().read(cx).content_origin().y;
        let item_top = (bounds.top() - origin_y).as_f32();
        let pad_rows = self.pane().read(cx).model.pad_rows;

        let viewport = ItemViewport {
            top: bounds.top().as_f32(),
            height: window.viewport_size().height.as_f32(),
            cell_height: self.cell().height_px,
            pad_rows,
        };

        match self {
            BlockListItem::Frozen {
                item_idx,
                store: _,
                cols: _,
                cell,
                selection,
                pane,
            } => {
                let model = &pane.read(cx).model;

                let mut view = model.source.frozen_block_view(
                    *item_idx,
                    &viewport,
                    *selection,
                    &model.duration_labels,
                    theme.foreground,
                );

                let record = FrameRecord::from_view(&view, item_top);

                pane.update(cx, |pane, _| pane.model.record_frame(record));

                view.items_chrome.clear();

                let shaped = shape_frozen_rows(&view.rows, cell.width_px, window);

                BlockListItemPrepaint::Frozen { view, shaped }
            }
            BlockListItem::Live {
                frame,
                history_rows,
                state,
                cols,
                cell,
                pane,
            } => {
                let tail_view = pane.read(cx).model.source.live_history_view(
                    *history_rows,
                    *cols,
                    &viewport,
                    theme.foreground,
                );

                let live_rows = frame_content_rows(frame);

                let layout =
                    state.layout(tail_view.active_top, live_rows, cell.height_px, pad_rows);

                let record = FrameRecord::from_live_view(&tail_view, &layout, item_top);

                pane.update(cx, |pane, _| pane.model.record_frame(record));

                let tail_shaped = shape_frozen_rows(&tail_view.rows, cell.width_px, window);

                let active_bounds = Bounds::new(
                    point(bounds.left(), bounds.top() + px(layout.active_top)),
                    size(bounds.size.width, px(layout.active_height)),
                );

                let active_shaped = shape_frame(active_bounds, frame, *cell, window);

                BlockListItemPrepaint::Live {
                    tail_view,
                    tail_shaped,
                    active_shaped,
                    active_bounds,
                }
            }
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Style,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let selection_bg = self.pane().read(cx).model.theme.selection_background;

        match (self, prepaint) {
            (
                BlockListItem::Frozen { cell, .. },
                BlockListItemPrepaint::Frozen { view, shaped },
            ) => {
                paint_frozen_images(bounds, view, *cell, window, false);

                paint_frozen(bounds, view, shaped, *cell, selection_bg, window, cx);

                paint_frozen_images(bounds, view, *cell, window, true);
            }
            (
                BlockListItem::Live { frame, cell, .. },
                BlockListItemPrepaint::Live {
                    tail_view,
                    tail_shaped,
                    active_shaped,
                    active_bounds,
                },
            ) => {
                paint_frozen(
                    bounds,
                    tail_view,
                    tail_shaped,
                    *cell,
                    selection_bg,
                    window,
                    cx,
                );

                paint_frame(
                    *active_bounds,
                    frame,
                    active_shaped.as_slice(),
                    *cell,
                    &[],
                    window,
                    cx,
                );
            }
            _ => {}
        }
    }
}

impl BlockListItem {
    fn cell(&self) -> metrics::CellMetrics {
        match self {
            Self::Frozen { cell, .. } | Self::Live { cell, .. } => *cell,
        }
    }

    fn pane(&self) -> &Entity<TerminalPane> {
        match self {
            BlockListItem::Frozen { pane, .. } | BlockListItem::Live { pane, .. } => pane,
        }
    }
}

/// Shape the visible frozen rows. Block rows cache by `(block_id,
/// generation, row)`; live-history rows hash their text.
fn shape_frozen_rows(rows: &[FrozenRow], cell_w: f32, window: &mut Window) -> Vec<ShapedLine> {
    shape_lines(
        rows.iter().map(|row| {
            (
                row.shape_key.unwrap_or_else(|| row.line.text_hash()),
                &row.line,
            )
        }),
        cell_w,
        window,
    )
}

/// Paint separators + frozen rows (backgrounds then glyphs).
fn paint_frozen(
    bounds: Bounds<Pixels>,
    view: &FrozenView,
    shaped: &[ShapedLine],
    cell: CellMetrics,
    selection_bg: TerminalColor,
    window: &mut Window,
    cx: &mut App,
) {
    for row in &view.rows {
        paint_line_backgrounds_at(bounds, &row.line, row.y, cell, window);
    }

    // Selection tint under the glyphs (over the cell backgrounds).

    for row in &view.rows {
        let Some((start, end)) = row.selected else {
            continue;
        };

        window.paint_quad(fill(
            Bounds::new(
                point(
                    bounds.left() + px(start as f32 * cell.width_px),
                    bounds.top() + px(row.y),
                ),
                size(px((end - start) as f32 * cell.width_px), px(cell.height_px)),
            ),
            rgb(selection_bg.into()),
        ));
    }

    paint_glyph_rows(
        bounds,
        view.rows
            .iter()
            .zip(shaped)
            .map(|(row, line)| (row.y, line)),
        cell.height_px,
        window,
        cx,
    );
}
