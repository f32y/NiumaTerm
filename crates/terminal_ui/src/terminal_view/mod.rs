use crate::paint::chrome::{paint_frozen_chrome, paint_frozen_separators};
mod item;

#[cfg(test)]
mod tests;

use std::panic;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, ElementInputHandler, Entity,
    FocusHandle, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, ShapedLine,
    Style, Window, relative, size,
};

use crate::frame::TerminalFrame;
use crate::metrics;
#[cfg(test)]
pub(crate) use crate::paint::frame::cursor_bounds;
use crate::paint::frame::{paint_frame, shape_frame};
pub(crate) use crate::terminal_view::item::BlockListItem;
use crate::view::TerminalPane;

/// The terminal viewport as a custom GPUI leaf element: prepaint shapes the
/// visible rows (multi-run, per-cell foreground), paint draws backgrounds, the
/// styled glyphs, and the cursor. Mirrors GPUI's `Canvas` element shape.
pub(crate) struct TerminalView {
    frame: TerminalFrame,
    cell: metrics::CellMetrics,
    focus: FocusHandle,
    pane: Entity<TerminalPane>,
}

impl TerminalView {
    pub(crate) fn new(
        frame: TerminalFrame,
        cell: metrics::CellMetrics,
        focus: FocusHandle,
        pane: Entity<TerminalPane>,
    ) -> Self {
        Self {
            frame,
            cell,
            focus,
            pane,
        }
    }
}

impl IntoElement for TerminalView {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

pub(crate) struct TerminalPrepaint {
    shaped: Vec<ShapedLine>,
    row_offsets: Arc<[f32]>,
}

impl Element for TerminalView {
    type RequestLayoutState = Style;
    type PrepaintState = TerminalPrepaint;

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
    ) -> TerminalPrepaint {
        // Feed the real content rect back to the pane so it resizes the surface to
        // its actual area (below the tab bar), not the full window.
        let cell = self.cell;

        let row_offsets = self.pane.update(cx, |pane, cx| {
            pane.set_content_bounds(bounds, cell, cx);
            pane.model.viewport.row_offsets()
        });

        TerminalPrepaint {
            shaped: shape_frame(bounds, &self.frame, self.cell, window),
            row_offsets,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Style,
        prepaint: &mut TerminalPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        paint_frame(
            bounds,
            &self.frame,
            prepaint.shaped.as_slice(),
            self.cell,
            &prepaint.row_offsets,
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
        let separators = pane.model.frozen.separators().to_vec();
        let chrome = pane.model.frozen.chrome().to_vec();

        if self.show_chrome {
            paint_frozen_separators(bounds, &separators, window);
        }

        self.list.paint(window, cx);

        if self.show_chrome {
            paint_frozen_chrome(bounds, &chrome, window, cx);
        }

        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, self.pane.clone()),
            cx,
        );
    }
}
