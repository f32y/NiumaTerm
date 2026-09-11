mod item;

#[cfg(test)]
mod tests;

use std::panic;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, ElementInputHandler, Entity,
    FocusHandle, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Rgba,
    ShapedLine, Style, TextAlign, TextRun, Window, fill, point, px, relative, rgb, rgba, size,
};

use crate::block_list::FrozenItemChrome;
use crate::frame::TerminalFrame;
use crate::metrics;
#[cfg(test)]
pub(crate) use crate::paint::frame::cursor_bounds;
use crate::paint::frame::{paint_frame, shape_frame};
pub(crate) use crate::terminal_view::item::BlockListItem;
use crate::theme::{BLOCK_GUTTER_GAP, BLOCK_GUTTER_WIDTH, BLOCK_SELECTED_TINT, SEPARATOR_COLOR};
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

fn paint_frozen_separators(bounds: Bounds<Pixels>, separators: &[f32], window: &mut Window) {
    let left = bounds.left() - px(metrics::PADDING_PX);
    let right = bounds.right() + px(metrics::PADDING_PX);
    for y in separators {
        window.paint_quad(fill(
            Bounds::new(
                point(left, bounds.top() + px(*y)),
                size(right - left, px(1.0)),
            ),
            Rgba {
                r: ((SEPARATOR_COLOR >> 16) & 0xff) as f32 / 255.0,
                g: ((SEPARATOR_COLOR >> 8) & 0xff) as f32 / 255.0,
                b: (SEPARATOR_COLOR & 0xff) as f32 / 255.0,
                a: 0.67,
            },
        ));
    }
}

fn paint_frozen_chrome(
    bounds: Bounds<Pixels>,
    items_chrome: &[FrozenItemChrome],
    window: &mut Window,
    cx: &mut App,
) {
    for chrome in items_chrome {
        let top = bounds.top() + px(chrome.top);
        let height = px(chrome.bottom - chrome.top);
        let gutter_alpha = if chrome.selected { 0xe6 } else { 0x59 };

        window.paint_quad(fill(
            Bounds::new(
                point(
                    bounds.left() - px(BLOCK_GUTTER_GAP + BLOCK_GUTTER_WIDTH),
                    top,
                ),
                size(px(BLOCK_GUTTER_WIDTH), height),
            ),
            rgba((chrome.accent << 8) | gutter_alpha),
        ));

        if chrome.selected {
            window.paint_quad(fill(
                Bounds::new(point(bounds.left(), top), size(bounds.size.width, height)),
                rgba(BLOCK_SELECTED_TINT),
            ));
        }
    }

    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());

    for chrome in items_chrome {
        let Some(header) = chrome.header.as_deref() else {
            continue;
        };

        let runs = [TextRun {
            len: header.len(),
            font: style.font(),
            color: rgb(0x7f8c98).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }];

        let shaped = window.text_system().shape_line(
            header.to_string().into(),
            font_size,
            &runs,
            Some(bounds.size.width),
        );

        let _ = shaped.paint(
            point(bounds.left(), bounds.top() + px(chrome.header_y)),
            px(0.0),
            TextAlign::Right,
            Some(bounds.size.width),
            window,
            cx,
        );
    }
}
