use std::{collections, panic, sync};

use gpui::{
    App, Bounds, Element, ElementId, Entity, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, ShapedLine, Style, Window, point, px, relative, size,
};
use nmt_terminal::block_store::BlockStore;
use parking_lot::Mutex;

use crate::block_list::block_list_live_chrome;
use crate::frame::TerminalFrame;
use crate::layout::frame_content_rows;
use crate::paint::blocks::{paint_frozen, shape_frozen_rows};
use crate::paint::frame::{paint_frame, paint_frozen_images, shape_frame};
use crate::pane_model::frame_record::FrameRecord;
use crate::session::InFlightBlock;
use crate::view::TerminalPane;
use crate::{block_list, metrics};

type SharedBlockStore = sync::Arc<Mutex<BlockStore>>;

pub(crate) enum BlockListItem {
    Frozen {
        item_idx: usize,
        store: SharedBlockStore,
        cols: u32,
        cell: metrics::CellMetrics,
        selection: Option<(block_list::FrozenPoint, block_list::FrozenPoint)>,
        selected_item: Option<usize>,
        pane: Entity<TerminalPane>,
    },
    Live {
        frame: TerminalFrame,
        /// Active-grid scrollback rows rendered above the live grid
        /// when scrolling into a running command.
        history_rows: u64,
        in_flight: Option<InFlightBlock>,
        has_open_prompt: bool,
        live_index: usize,
        selected_item: Option<usize>,
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

        let pad_rows = self.pane().read(cx).model.settings.pad_rows;

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
        let pad_rows = self.pane().read(cx).model.settings.pad_rows;

        match self {
            BlockListItem::Frozen {
                item_idx,
                store: _,
                cols: _,
                cell,
                selection,
                selected_item,
                pane,
            } => {
                let snapshot = pane.read(cx).model.source.session.block_snapshot(*item_idx);
                let mut view = match snapshot.and_then(|(item, acquired)| {
                    block_list::handle_item_info(&item, &pane.read(cx).model.duration_labels)
                        .map(|info| (info, acquired))
                }) {
                    Some((info, acquired)) => {
                        let visible = block_list::visible_rows(
                            bounds.top().as_f32(),
                            info.rows,
                            window.viewport_size().height.as_f32(),
                            cell.height_px,
                            pad_rows,
                        );

                        let mut view = block_list::frozen_block_view(
                            acquired.as_ref().map(|acq| (&acq.block, &acq.palette)),
                            &info,
                            *item_idx,
                            visible.clone(),
                            cell.height_px,
                            pad_rows,
                            *selection,
                            *selected_item,
                            theme.foreground,
                        );

                        // Resolve each frozen Kitty placement's
                        // generation from the session's (block_id, image_id)
                        // cache; misses read pixels out of the acquired block
                        // once and land in the cache for later frames.
                        if let Some(acq) = &acquired
                            && !acq.placements.is_empty()
                        {
                            let ids: collections::HashSet<u32> =
                                acq.placements.iter().map(|p| p.image_id).collect();

                            let surface = &pane.read(cx).model.source;

                            let generations: collections::HashMap<_, _> = ids
                                .into_iter()
                                .filter_map(|id| {
                                    surface
                                        .frozen_image(&acq.block, id)
                                        .map(|generation| (id, generation))
                                })
                                .collect();

                            view.images = block_list::frozen_block_images(
                                &acq.placements,
                                &generations,
                                &visible,
                                cell.height_px,
                                pad_rows,
                            );
                        }

                        view
                    }
                    None => Default::default(),
                };

                let record = FrameRecord::from_view(&view, item_top);
                pane.update(cx, |pane, _| pane.model.record_frame(record));

                view.items_chrome.clear();

                let shaped = shape_frozen_rows(&view.rows, cell.width_px, window);

                BlockListItemPrepaint::Frozen { view, shaped }
            }
            BlockListItem::Live {
                frame,
                history_rows,
                in_flight,
                has_open_prompt,
                live_index,
                selected_item,
                cols,
                cell,
                pane,
            } => {
                // The active grid's scrollback rows render above the live
                // grid, visible range only; a running command's
                // scroll-up history).
                let tail_view = {
                    let visible = block_list::visible_rows(
                        bounds.top().as_f32(),
                        (*history_rows).min(usize::MAX as u64) as usize,
                        window.viewport_size().height.as_f32(),
                        cell.height_px,
                        pad_rows,
                    );

                    let pane = pane.read(cx);

                    let lines = pane.model.source.live_history_lines(
                        visible.start as u64..visible.end as u64,
                        theme.foreground,
                    );
                    let selection = pane.model.source.session.selection_screen_range();

                    block_list::live_history_view(
                        lines,
                        *history_rows,
                        *cols,
                        cell.height_px,
                        pad_rows,
                        selection,
                    )
                };

                let live_rows = frame_content_rows(frame);

                let live_chrome = block_list_live_chrome(
                    *live_index,
                    live_rows,
                    cell.height_px,
                    in_flight.as_ref(),
                    *has_open_prompt,
                    *selected_item == Some(*live_index),
                );

                let mut record = FrameRecord::from_view(&tail_view, item_top);
                record.active_top = Some(item_top + tail_view.active_top);
                if let Some(mut chrome) = live_chrome {
                    chrome.bottom = tail_view.active_top
                        + live_rows as f32 * cell.height_px
                        + pad_rows * cell.height_px;
                    chrome.header_y = tail_view.active_top;
                    record.push_chrome(chrome, item_top);
                }
                pane.update(cx, |pane, _| pane.model.record_frame(record));

                let tail_shaped = shape_frozen_rows(&tail_view.rows, cell.width_px, window);

                let active_bounds = Bounds::new(
                    point(bounds.left(), bounds.top() + px(tail_view.active_top)),
                    size(bounds.size.width, px(live_rows as f32 * cell.height_px)),
                );

                let active_shaped = shape_frame(active_bounds, frame, *cell, window);

                BlockListItemPrepaint::Live {
                    tail_view,
                    tail_shaped,
                    active_shaped,
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

                let active_bounds = Bounds::new(
                    point(bounds.left(), bounds.top() + px(tail_view.active_top)),
                    size(
                        bounds.size.width,
                        px(active_shaped.len() as f32 * cell.height_px),
                    ),
                );

                paint_frame(
                    active_bounds,
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
    fn pane(&self) -> &Entity<TerminalPane> {
        match self {
            BlockListItem::Frozen { pane, .. } | BlockListItem::Live { pane, .. } => pane,
        }
    }
}
