use std::{collections, panic, sync};

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, ContentMask, Corners, Element, ElementId,
    ElementInputHandler, Entity, FocusHandle, FontStyle, FontWeight, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, RenderImage, ShapedLine, StrikethroughStyle,
    Style, TextAlign, TextRun, UnderlineStyle, Window, fill, point, px, relative, rgb, size,
};
use nmt_terminal::ansi::CursorShape;
use nmt_terminal::block_store::BlockStore;
use nmt_terminal::terminal::square::Wide;
use parking_lot::Mutex;

use super::block_list::{block_list_live_chrome, block_pad_rows};
use super::frame::{TerminalColor, TerminalCursor, TerminalFrame, TerminalLine};
use super::metrics;
use super::session::InFlightBlock;
use super::view::TerminalPane;
use crate::terminal;

/// The terminal viewport as a custom GPUI leaf element: prepaint shapes the
/// visible rows (multi-run, per-cell foreground), paint draws backgrounds, the
/// styled glyphs, and the cursor. Mirrors GPUI's `Canvas` element shape.
pub(crate) struct TerminalView {
    frame: TerminalFrame,
    cell: metrics::CellMetrics,
    focus: FocusHandle,
    pane: Entity<TerminalPane>,
    /// FixedBottom input style: bottom-anchor the grid so the last content row
    /// pins to the viewport floor (Warp parity, incl. interactive output).
    fixed_bottom: bool,
}

impl TerminalView {
    pub(crate) fn new(
        frame: TerminalFrame,
        cell: metrics::CellMetrics,
        focus: FocusHandle,
        pane: Entity<TerminalPane>,
        fixed_bottom: bool,
    ) -> Self {
        Self {
            frame,
            cell,
            focus,
            pane,
            fixed_bottom,
        }
    }
}

impl IntoElement for TerminalView {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for TerminalView {
    type RequestLayoutState = Style;
    type PrepaintState = Vec<ShapedLine>;

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
        style.size.height = relative(1.0).into();

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
    ) -> Vec<ShapedLine> {
        // Feed the real content rect back to the pane so it resizes the surface to
        // its actual area (below the tab bar), not the full window.
        let cell = self.cell;

        self.pane
            .update(cx, |pane, cx| pane.set_content_bounds(bounds, cell, cx));

        shape_frame(bounds, &self.frame, self.cell, window)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Style,
        prepaint: &mut Vec<ShapedLine>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let offsets = bottom_anchor_offsets(&self.frame, self.cell.height_px, self.fixed_bottom);

        paint_frame(
            bounds,
            &self.frame,
            prepaint.as_slice(),
            self.cell,
            &offsets,
            window,
            cx,
        );

        // Register commit-only IME for the focused pane; self-gates on focus.
        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, self.pane.clone()),
            cx,
        );
    }
}

/// Block-split list wrapper: the child is a real `gpui::list`; this wrapper
/// only feeds pane bounds, paints chrome that extends into the left padding,
/// and keeps the IME handler attached to the full terminal content rect.
pub(crate) struct BlockListView {
    pub(crate) cell: metrics::CellMetrics,
    pub(crate) focus: FocusHandle,
    pub(crate) pane: Entity<TerminalPane>,
    pub(crate) list: AnyElement,
    pub(crate) show_chrome: bool,
}

impl IntoElement for BlockListView {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for BlockListView {
    type RequestLayoutState = Style;
    type PrepaintState = ();

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
        style.size.height = relative(1.0).into();

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
    ) {
        let cell = self.cell;

        self.pane
            .update(cx, |pane, cx| pane.begin_block_list_frame(bounds, cell, cx));

        self.list.layout_as_root(
            size(
                AvailableSpace::Definite(bounds.size.width),
                AvailableSpace::Definite(bounds.size.height),
            ),
            window,
            cx,
        );

        self.list.prepaint_at(bounds.origin, window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Style,
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let pane = self.pane.read(cx);
        let separators = pane.frozen_separators.clone();
        let chrome = pane.frozen_chrome.clone();

        if self.show_chrome {
            terminal::block_list::paint_frozen_separators(bounds, &separators, window);
        }

        self.list.paint(window, cx);

        if self.show_chrome {
            terminal::block_list::paint_frozen_chrome(bounds, &chrome, window, cx);
        }

        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, self.pane.clone()),
            cx,
        );
    }
}

type SharedBlockStore = sync::Arc<Mutex<BlockStore>>;

pub(crate) enum BlockListItem {
    Frozen {
        item_idx: usize,
        store: SharedBlockStore,
        cols: u32,
        cell: metrics::CellMetrics,
        selection: Option<(
            terminal::block_list::FrozenPoint,
            terminal::block_list::FrozenPoint,
        )>,
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
        view: terminal::block_list::FrozenView,
        shaped: Vec<ShapedLine>,
    },
    Live {
        tail_view: terminal::block_list::FrozenView,
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

        let pad_rows = block_pad_rows(cx);

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
                    .map(|item| {
                        terminal::block_list::item_px(item, *cols, cell.height_px, pad_rows)
                    })
                    .unwrap_or(0.0)
            }
            BlockListItem::Live {
                frame,
                history_rows,
                cell,
                ..
            } => terminal::block_list::live_item_px(
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
        let origin_y = self.pane().read(cx).content_origin().y;
        let item_top = (bounds.top() - origin_y).as_f32();
        let pad_rows = block_pad_rows(cx);

        match self {
            BlockListItem::Frozen {
                item_idx,
                store,
                cols: _,
                cell,
                selection,
                selected_item,
                pane,
            } => {
                // Snapshot the item under the store lock, then release it
                // before touching the engine — the PTY thread nests engine →
                // store, so the reverse nesting here would deadlock.
                let handle_info = {
                    let store = store.lock();
                    store
                        .items()
                        .get(*item_idx)
                        .and_then(terminal::block_list::handle_item_info)
                };

                let mut view = match handle_info {
                    Some(info) => {
                        let visible = terminal::block_list::visible_rows(
                            bounds.top().as_f32(),
                            info.rows,
                            window.viewport_size().height.as_f32(),
                            cell.height_px,
                            pad_rows,
                        );

                        let acquired = pane.read(cx).surface.acquire_block(info.handle);

                        let mut view = terminal::block_list::frozen_block_view(
                            acquired.as_ref().map(|acq| (&acq.block, &acq.palette)),
                            &info,
                            *item_idx,
                            visible.clone(),
                            cell.height_px,
                            pad_rows,
                            *selection,
                            *selected_item,
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

                            let surface = &pane.read(cx).surface;

                            let generations: collections::HashMap<_, _> = ids
                                .into_iter()
                                .filter_map(|id| {
                                    surface
                                        .frozen_image(info.handle.id, id)
                                        .or_else(|| {
                                            let generation =
                                                surface.frozen_image_generation(&acq.block, id)?;
                                            surface.insert_frozen_image(
                                                info.handle.id,
                                                id,
                                                generation.clone(),
                                            );
                                            Some(generation)
                                        })
                                        .map(|generation| (id, generation))
                                })
                                .collect();

                            view.images = terminal::block_list::frozen_block_images(
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

                pane.update(cx, |pane, _| pane.record_frozen_view(&view, item_top));

                view.items_chrome.clear();

                let shaped =
                    terminal::block_list::shape_frozen_rows(&view.rows, cell.width_px, window);

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
                    let visible = terminal::block_list::visible_rows(
                        bounds.top().as_f32(),
                        (*history_rows).min(usize::MAX as u64) as usize,
                        window.viewport_size().height.as_f32(),
                        cell.height_px,
                        pad_rows,
                    );

                    let pane = pane.read(cx);

                    let lines = pane
                        .surface
                        .live_history_lines(visible.start as u64..visible.end as u64);
                    let selection = pane.surface.selection_screen_range();

                    terminal::block_list::live_history_view(
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

                pane.update(cx, |pane, _| {
                    pane.record_frozen_view(&tail_view, item_top);

                    let active_top = item_top + tail_view.active_top;

                    pane.frozen_hit.set_active_top(active_top);

                    if let Some(mut chrome) = live_chrome {
                        chrome.bottom = tail_view.active_top
                            + live_rows as f32 * cell.height_px
                            + pad_rows * cell.height_px;

                        chrome.header_y = tail_view.active_top;

                        pane.record_frozen_chrome(chrome, item_top);
                    }
                });

                let tail_shaped =
                    terminal::block_list::shape_frozen_rows(&tail_view.rows, cell.width_px, window);

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
        match (self, prepaint) {
            (
                BlockListItem::Frozen { cell, .. },
                BlockListItemPrepaint::Frozen { view, shaped },
            ) => {
                paint_frozen_images(bounds, view, *cell, window, false);

                terminal::block_list::paint_frozen(
                    bounds,
                    view,
                    shaped,
                    cell.width_px,
                    cell.height_px,
                    window,
                    cx,
                );

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
                terminal::block_list::paint_frozen(
                    bounds,
                    tail_view,
                    tail_shaped,
                    cell.width_px,
                    cell.height_px,
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

pub(crate) fn frame_content_rows(frame: &TerminalFrame) -> usize {
    let lines = frame.lines();

    let mut content_end = 0;

    for (row, line) in lines.iter().enumerate().rev() {
        if terminal_line_has_content(line) {
            content_end = row + 1;
            break;
        }
    }

    if let Some(cursor) = frame.cursor() {
        content_end = content_end.max(cursor.row as usize + 1);
    }

    content_end.min(lines.len())
}

pub(crate) fn bottom_anchor_offsets(
    frame: &TerminalFrame,
    cell_height: f32,
    fixed_bottom: bool,
) -> Vec<f32> {
    if !fixed_bottom {
        return Vec::new();
    }

    let rows = frame.lines().len();

    let slack = rows.saturating_sub(frame_content_rows(frame)) as f32 * cell_height;

    if slack > 0.0 {
        vec![slack; rows]
    } else {
        Vec::new()
    }
}

pub(crate) fn live_frame_text(frame: &TerminalFrame) -> Option<String> {
    let rows = frame_content_rows(frame);

    if rows == 0 {
        return None;
    }

    let mut lines = frame
        .lines()
        .iter()
        .take(rows)
        .map(terminal_line_plain_text)
        .collect::<Vec<_>>();

    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }

    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn terminal_line_plain_text(line: &TerminalLine) -> String {
    line.text().replace('\u{00a0}', " ").trim_end().to_string()
}

fn terminal_line_has_content(line: &TerminalLine) -> bool {
    line.cells()
        .iter()
        .any(|c| !matches!(c.ch, '\0' | ' ' | '\u{00a0}'))
}

/// The pixel y-offset for a viewport row (0 with no gaps / out of range).
pub(crate) fn row_y_offset(offsets: &[f32], row: usize) -> f32 {
    offsets.get(row).copied().unwrap_or(0.0)
}

/// Inverse of the offset mapping: the viewport row under a content-relative
/// pixel y. A pointer inside the gap above a block maps to the block's first row.
pub(crate) fn terminal_row_at_y(y: f32, cell_height: f32, offsets: &[f32]) -> u16 {
    if offsets.is_empty() {
        return (y / cell_height).floor().max(0.0) as u16;
    }

    for (row, off) in offsets.iter().enumerate() {
        if y < (row as f32 + 1.0) * cell_height + off {
            return row as u16;
        }
    }

    offsets.len().saturating_sub(1) as u16
}

pub(crate) const BLOCK_SUCCESS_COLOR: u32 = 0xa3be8c;
pub(crate) const BLOCK_FAILURE_COLOR: u32 = 0xbf616a;
pub(crate) const BLOCK_RUNNING_COLOR: u32 = 0x88c0d0;
pub(crate) const BLOCK_INPUT_COLOR: u32 = 0xebcb8b;
pub(crate) const BLOCK_SELECTED_TINT: u32 = 0xffffff0d;

pub(crate) fn block_separator_bounds(
    bounds: Bounds<Pixels>,
    y: Pixels,
    thickness: f32,
) -> Bounds<Pixels> {
    let left = bounds.left() - px(metrics::PADDING_PX);
    let right = bounds.right() + px(metrics::PADDING_PX);

    Bounds::new(point(left, y), size(right - left, px(thickness)))
}

/// First `max` chars of the command for the header label.
pub(crate) fn truncate_command(command: &str, max: usize) -> String {
    if command.chars().count() <= max {
        command.to_string()
    } else {
        let head: String = command.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

fn shape_frame(
    bounds: Bounds<Pixels>,
    frame: &TerminalFrame,
    cell: metrics::CellMetrics,
    window: &mut Window,
) -> Vec<ShapedLine> {
    let row_count =
        ((bounds.size.height.as_f32() / cell.height_px).ceil() as usize).min(frame.lines().len());

    shape_lines(
        frame
            .lines()
            .iter()
            .take(row_count)
            .map(|line| (line.text_hash(), line)),
        cell.width_px,
        window,
    )
}

/// Shape terminal lines with per-cell forced width, cached by the caller's
/// key — the one shaping path for live-frame rows and frozen block rows.
pub(crate) fn shape_lines<'a>(
    lines: impl Iterator<Item = (u64, &'a TerminalLine)>,
    cell_w: f32,
    window: &mut Window,
) -> Vec<ShapedLine> {
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());
    let base = style.to_run(0);

    lines
        .map(|(key, line)| {
            let runs = terminal_text_runs(line, &base);
            window.text_system().shape_line_by_hash(
                key,
                line.text().len(),
                font_size,
                &runs,
                Some(px(cell_w)),
                || line.text().clone(),
            )
        })
        .collect()
}

/// Build one styled `TextRun` per foreground run, inheriting font/size from the
/// base run and overriding only the color. Run byte-lengths sum to the row text.
pub(crate) fn terminal_text_runs(line: &TerminalLine, base: &TextRun) -> Vec<TextRun> {
    if line.runs().is_empty() {
        // Blank row: keep the single zero/whitespace run path GPUI already handles.
        let mut run = base.clone();

        run.len = line.text().len();

        return vec![run];
    }

    line.runs()
        .iter()
        .map(|run| {
            let mut text_run = base.clone();

            text_run.len = run.len;
            text_run.color = rgb(run.fg.rgb_u32()).into();

            if run.bold {
                text_run.font.weight = FontWeight::BOLD;
            }

            if run.italic {
                text_run.font.style = FontStyle::Italic;
            }

            if run.underline {
                text_run.underline = Some(UnderlineStyle {
                    thickness: px(1.0),
                    color: None,
                    wavy: false,
                });
            }

            if run.strikethrough {
                text_run.strikethrough = Some(StrikethroughStyle {
                    thickness: px(1.0),
                    color: None,
                });
            }

            text_run
        })
        .collect()
}

fn paint_frame(
    bounds: Bounds<Pixels>,
    frame: &TerminalFrame,
    lines: &[ShapedLine],
    cell: metrics::CellMetrics,
    offsets: &[f32],
    window: &mut Window,
    cx: &mut App,
) {
    use crate::terminal::frame::ZLayer;

    // Kitty images below cell backgrounds (z < i32::MIN/2).
    paint_frame_images(
        bounds,
        frame,
        ZLayer::BelowBackground,
        cell,
        offsets,
        window,
    );

    for (row, line) in frame.lines().iter().take(lines.len()).enumerate() {
        paint_line_backgrounds_at(
            bounds,
            line,
            row as f32 * cell.height_px + row_y_offset(offsets, row),
            cell.width_px,
            cell.height_px,
            window,
        );
    }

    // Kitty images above backgrounds, below cursor/text (i32::MIN/2 <= z < 0).
    paint_frame_images(bounds, frame, ZLayer::BelowText, cell, offsets, window);
    paint_cursor(bounds, frame.cursor(), cell, offsets, window);

    paint_glyph_rows(
        bounds,
        lines.iter().enumerate().map(|(row, line)| {
            (
                row as f32 * cell.height_px + row_y_offset(offsets, row),
                line,
            )
        }),
        cell.height_px,
        window,
        cx,
    );

    // Kitty images above cursor/text (z >= 0).
    paint_frame_images(bounds, frame, ZLayer::AboveText, cell, offsets, window);
}

/// Paint shaped glyph rows at caller-supplied element-local y offsets — the
/// one glyph-paint convention (left-aligned, no wrap, cell-height lines) for
/// live-frame rows and frozen block rows.
pub(crate) fn paint_glyph_rows<'a>(
    bounds: Bounds<Pixels>,
    rows: impl Iterator<Item = (f32, &'a ShapedLine)>,
    cell_h: f32,
    window: &mut Window,
    cx: &mut App,
) {
    for (y, line) in rows {
        let _ = line.paint(
            point(bounds.left(), bounds.top() + px(y)),
            px(cell_h),
            TextAlign::Left,
            None,
            window,
            cx,
        );
    }
}

/// Paint the frame's Kitty images whose z-index falls in `layer`, in engine order (no
/// per-paint sort or descriptor allocation). Each image's full texture is painted into
/// the source-expanded bounds and clipped to its destination by a content mask, so a
/// source crop needs no CPU cropping. A painted generation is marked
/// uploaded so its atlas tile is released once its last reference drops.
fn paint_frame_images(
    bounds: Bounds<Pixels>,
    frame: &TerminalFrame,
    layer: terminal::frame::ZLayer,
    cell: metrics::CellMetrics,
    offsets: &[f32],
    window: &mut Window,
) {
    let images = frame.images();

    if images.is_empty() {
        return; // no graphics: zero work
    }

    for img in images {
        if img.z_layer() != layer {
            continue;
        }

        let top = img.top_row();
        let row_offset = if top >= 0 {
            row_y_offset(offsets, top as usize)
        } else {
            0.0
        };

        let Some((dest, source)) = img.destination(
            cell.width_px,
            cell.height_px,
            f32::from(bounds.left()),
            f32::from(bounds.top()),
            row_offset,
        ) else {
            continue;
        };

        paint_generation(window, dest, source, &img.generation);
    }
}

/// Paint a block-list item's frozen Kitty image slices whose z-layer is on
/// the requested side of the frozen text: `above_text == false` paints the below-text
/// slices (before `paint_frozen`), `true` the above-text slices (after). Uses the same
/// source-crop primitive as live images; clips to each slice's destination cell rect.
fn paint_frozen_images(
    bounds: Bounds<Pixels>,
    view: &terminal::block_list::FrozenView,
    cell: metrics::CellMetrics,
    window: &mut Window,
    above_text: bool,
) {
    if view.images.is_empty() {
        return;
    }

    for img in &view.images {
        if (img.z >= 0) != above_text {
            continue;
        }

        let dest = [
            f32::from(bounds.left()) + img.col as f32 * cell.width_px,
            f32::from(bounds.top()) + img.y,
            img.width as f32 * cell.width_px,
            cell.height_px,
        ];

        paint_generation(window, dest, img.source, &img.generation);
    }
}

/// Paint one image generation's `source` crop into `dest` and mark it
/// uploaded (its atlas tile releases with the last reference) — the shared
/// tail of live-frame and frozen image painting. Degenerate crops are
/// skipped.
fn paint_generation(
    window: &mut Window,
    dest: [f32; 4],
    source: [f32; 4],
    generation: &terminal::graphics::ImageGeneration,
) {
    let Some(full) = terminal::graphics::expanded_full_bounds(dest, source) else {
        return;
    };

    paint_image_clipped(window, dest, full, generation.image().clone());

    generation.mark_uploaded();
}

/// Paint `image`'s full texture into `full` bounds, clipped to `dest` — the source-crop
/// primitive. GPUI intersects the mask with the element's existing overflow
/// mask, so viewport clipping is automatic.
fn paint_image_clipped(
    window: &mut Window,
    dest: [f32; 4],
    full: [f32; 4],
    image: sync::Arc<RenderImage>,
) {
    let to_bounds = |b: [f32; 4]| Bounds {
        origin: point(px(b[0]), px(b[1])),
        size: size(px(b[2]), px(b[3])),
    };

    let mask = ContentMask {
        bounds: to_bounds(dest),
    };

    window.with_content_mask(Some(mask), |w| {
        let _ = w.paint_image(to_bounds(full), Corners::default(), image, 0, false);
    });
}

/// Paint one line's background color runs at an element-local pixel `y`,
/// merging contiguous cells of equal background into single quads. Shared by
/// the live grid and the frozen block rows.
pub(crate) fn paint_line_backgrounds_at(
    bounds: Bounds<Pixels>,
    line: &TerminalLine,
    y: f32,
    cell_w: f32,
    cell_h: f32,
    window: &mut Window,
) {
    let mut run_start = 0u16;
    let mut run_width = 0u16;
    let mut run_color: Option<TerminalColor> = None;

    let flush = |start: u16, width: u16, color: Option<TerminalColor>, window: &mut Window| {
        let Some(color) = color else { return };

        if width == 0 {
            return;
        }

        window.paint_quad(fill(
            Bounds::new(
                point(
                    bounds.left() + px(start as f32 * cell_w),
                    bounds.top() + px(y),
                ),
                size(px(width as f32 * cell_w), px(cell_h)),
            ),
            rgb(color.rgb_u32()),
        ));
    };
    for cell_data in line.cells() {
        let width: u16 = if cell_data.wide == Wide::Wide { 2 } else { 1 };

        if run_color == cell_data.background && run_start + run_width == cell_data.col {
            run_width += width;
            continue;
        }

        flush(run_start, run_width, run_color, window);

        run_start = cell_data.col;
        run_width = width;
        run_color = cell_data.background;
    }

    flush(run_start, run_width, run_color, window);
}

fn paint_cursor(
    bounds: Bounds<Pixels>,
    cursor: Option<TerminalCursor>,
    cell: metrics::CellMetrics,
    offsets: &[f32],
    window: &mut Window,
) {
    let Some(cursor) = cursor else {
        return;
    };

    let y_offset = row_y_offset(offsets, cursor.row as usize);

    let Some(bounds) = cursor_bounds(bounds, cursor, cell, y_offset) else {
        return;
    };

    window.paint_quad(fill(bounds, rgb(cursor.color.rgb_u32())));
}

pub(crate) fn cursor_bounds(
    bounds: Bounds<Pixels>,
    cursor: TerminalCursor,
    cell: metrics::CellMetrics,
    y_offset: f32,
) -> Option<Bounds<Pixels>> {
    let x = bounds.left() + px(cursor.col as f32 * cell.width_px);
    let y = bounds.top() + px(cursor.row as f32 * cell.height_px + y_offset);
    let thickness = px((cell.width_px.min(cell.height_px) / 8.0)
        .round()
        .clamp(1.0, 2.0));

    Some(match cursor.shape {
        CursorShape::Block => Bounds::new(point(x, y), size(px(cell.width_px), px(cell.height_px))),
        CursorShape::Beam => Bounds::new(point(x, y), size(thickness, px(cell.height_px))),
        CursorShape::Underline => Bounds::new(
            point(x, y + px(cell.height_px) - thickness),
            size(px(cell.width_px), thickness),
        ),
        CursorShape::Hidden => return None,
    })
}

#[cfg(test)]
mod tests {
    use gpui::{Bounds, point, px, size};
    use nmt_terminal::ansi::CursorShape;
    use nmt_terminal::render_buffer::RenderBuffer;

    use super::{bottom_anchor_offsets, cursor_bounds, metrics};
    use crate::terminal;
    use crate::terminal::frame::TerminalCursor;

    #[test]
    fn bottom_anchor_offsets_pin_content_to_the_floor() {
        let frame = terminal::frame::TerminalFrame::from_render_buffer(&RenderBuffer::new(80, 3));

        assert_eq!(
            bottom_anchor_offsets(&frame, 10.0, false),
            Vec::<f32>::new()
        );
        assert_eq!(bottom_anchor_offsets(&frame, 10.0, true), [30.0; 3]);
    }

    /// The gutter hit band covers the strip painted in the left padding plus a
    /// small tolerance into column 0 — and nothing else.

    #[test]
    fn cursor_bounds_cover_block_beam_and_underline() {
        let bounds = Bounds::new(point(px(10.0), px(20.0)), size(px(100.0), px(100.0)));
        let cell = metrics::CellMetrics {
            width_px: 8.0,
            height_px: 18.0,
        };

        let block = cursor_bounds(
            bounds,
            TerminalCursor {
                col: 2,
                row: 1,
                shape: CursorShape::Block,
                color: (0, 0, 0).into(),
            },
            cell,
            0.0,
        )
        .unwrap();
        assert_eq!(block.origin, point(px(26.0), px(38.0)));
        assert_eq!(block.size, size(px(8.0), px(18.0)));

        let beam = cursor_bounds(
            bounds,
            TerminalCursor {
                col: 2,
                row: 1,
                shape: CursorShape::Beam,
                color: (0, 0, 0).into(),
            },
            cell,
            0.0,
        )
        .unwrap();
        assert_eq!(beam.size.width, px(1.0));
        assert_eq!(beam.size.height, px(18.0));

        let underline = cursor_bounds(
            bounds,
            TerminalCursor {
                col: 2,
                row: 1,
                shape: CursorShape::Underline,
                color: (0, 0, 0).into(),
            },
            cell,
            0.0,
        )
        .unwrap();
        assert_eq!(underline.origin.y, px(55.0));
        assert_eq!(underline.size, size(px(8.0), px(1.0)));
    }
}
