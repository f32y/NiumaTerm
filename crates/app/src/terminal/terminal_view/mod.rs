mod item;
mod paint;

#[cfg(test)]
mod tests;

use std::{collections, panic, sync};

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, ContentMask, Corners, Element, ElementId,
    ElementInputHandler, Entity, FocusHandle, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, RenderImage, ShapedLine, Style, Window, fill, point, px, relative, rgb, size,
};
use nmt_terminal::ansi::CursorShape;
use nmt_terminal::block_store::BlockStore;
use parking_lot::Mutex;

use crate::terminal;
use crate::terminal::block_list::{block_list_live_chrome, block_pad_rows};
use crate::terminal::frame::{TerminalCursor, TerminalFrame};
use crate::terminal::layout::{bottom_anchor_offsets, frame_content_rows, row_y_offset};
use crate::terminal::metrics;
use crate::terminal::paint_text::{paint_glyph_rows, paint_line_backgrounds_at, shape_lines};
use crate::terminal::session::InFlightBlock;
pub(crate) use crate::terminal::terminal_view::item::BlockListItem;
#[allow(unused_imports)]
pub(crate) use crate::terminal::terminal_view::item::BlockListItemPrepaint;
#[allow(unused_imports)]
pub(crate) use crate::terminal::terminal_view::paint::cursor_bounds;
use crate::terminal::terminal_view::paint::{paint_frame, shape_frame};
use crate::terminal::view::TerminalPane;

/// The terminal viewport as a custom GPUI leaf element: prepaint shapes the
/// visible rows (multi-run, per-cell foreground), paint draws backgrounds, the
/// styled glyphs, and the cursor. Mirrors GPUI's `Canvas` element shape.
pub(crate) struct TerminalView {
    frame: TerminalFrame,
    cell: metrics::CellMetrics,
    focus: FocusHandle,
    pane: Entity<TerminalPane>,
    /// FixedBottom input style: bottom-anchor the grid so the last content row
    /// pins to the viewport floor to match Warp, including interactive output.
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
