use gpui::Corners;
use std::{
    ops::Range,
    rc::Rc,
    sync::{Arc, Mutex},
};

use gpui::{
    App, BorderStyle, Bounds, CursorStyle, Edges, Element, ElementId, GlobalElementId, Half,
    HighlightStyle, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString, StyledText,
    TextLayout, UnderlineStyle, Window, point, px, quad,
};

use crate::{
    ActiveTheme, WindowExt as _,
    global_state::GlobalState,
    input::Selection,
    text::LinkClickHandlerFn,
    text::TextViewMultiClickKind,
    text::node::{LinkMark, open_link_target},
    text::selection::word_range_at,
};

/// Where the character at `offset` is actually painted, and how wide it is.
fn char_cell(
    text_layout: &TextLayout,
    offset: usize,
    c: char,
    line_height: Pixels,
) -> Option<(Point<Pixels>, Pixels)> {
    let pos = text_layout.position_for_index(offset)?;
    let next_pos = text_layout.position_for_index(offset + c.len_utf8());
    Some(char_cell_at(
        pos,
        next_pos,
        text_layout.bounds().left(),
        c,
        line_height,
    ))
}

/// Resolve a character cell from the layout position of the character and of
/// the one after it.
///
/// A soft-wrap boundary index is reported at the end of the row the text
/// wrapped out of, so the first character of every wrapped row would otherwise
/// be tested against the previous row's geometry: a selection confined to one
/// row never reaches that phantom position and the leftmost character silently
/// drops out. A following position on another row means this character starts a
/// row, so anchor it to `left`, the row's left edge, where it is drawn. A hard
/// line break keeps the reported end-of-row position; it is drawn nowhere and
/// must not claim the next row's first cell.
///
/// The width falls back to half the line height when the following character
/// has no position at all (this one is the last the layout knows about).
fn char_cell_at(
    pos: Point<Pixels>,
    next_pos: Option<Point<Pixels>>,
    left: Pixels,
    c: char,
    line_height: Pixels,
) -> (Point<Pixels>, Pixels) {
    let Some(next_pos) = next_pos else {
        return (pos, line_height.half());
    };
    if next_pos.y == pos.y {
        return (pos, next_pos.x - pos.x);
    }
    if c == '\n' || c == '\r' {
        return (pos, line_height.half());
    }

    (point(left, next_pos.y), next_pos.x - left)
}

/// A inline element used to render a inline text and support selectable.
///
/// All text in TextView (including the CodeBlock) used this for text rendering.
pub(super) struct Inline {
    id: ElementId,
    text: SharedString,
    links: Rc<Vec<(Range<usize>, LinkMark)>>,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    styled_text: StyledText,
    link_click_handler: Option<Arc<LinkClickHandlerFn>>,

    state: Arc<Mutex<InlineState>>,
}

/// The inline text state, used RefCell to keep the selection state.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct InlineState {
    hovered_link: Option<SharedString>,
    /// The text that actually rendering, matched with selection.
    pub(super) text: SharedString,
    pub(super) selection: Option<Selection>,
}

impl InlineState {
    /// Save actually rendered text for selected text to use.
    pub(crate) fn set_text(&mut self, text: SharedString) {
        self.text = text;
    }
}

impl Inline {
    pub(super) fn new(
        id: impl Into<ElementId>,
        state: Arc<Mutex<InlineState>>,
        links: Vec<(Range<usize>, LinkMark)>,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
        link_click_handler: Option<Arc<LinkClickHandlerFn>>,
    ) -> Self {
        let text = state
            .lock()
            .map(|state| state.text.clone())
            .unwrap_or_default();

        Self {
            id: id.into(),
            links: Rc::new(links),
            highlights,
            text: text.clone(),
            styled_text: StyledText::new(text),
            link_click_handler,
            state,
        }
    }

    /// Get link at given mouse position.
    fn link_for_position(
        layout: &TextLayout,
        links: &Vec<(Range<usize>, LinkMark)>,
        position: Point<Pixels>,
    ) -> Option<LinkMark> {
        let offset = layout.index_for_position(position).ok()?;
        for (range, link) in links.iter() {
            if range.contains(&offset) {
                return Some(link.clone());
            }
        }

        None
    }

    /// Paint selected bounds for debug.
    #[allow(unused)]
    fn paint_selected_bounds(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        window.paint_quad(gpui::PaintQuad {
            bounds,
            background: cx.theme().blue.alpha(0.01).into(),
            corner_radii: Corners::default(),
            border_color: gpui::transparent_black(),
            border_style: BorderStyle::default(),
            border_widths: gpui::Edges::all(px(0.)),
        });
    }

    fn layout_selections(
        &self,
        text_layout: &TextLayout,
        bounds: &Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> (bool, bool, Option<Selection>) {
        let Some(text_view_state) = GlobalState::global(cx).text_view_state() else {
            return (false, false, None);
        };

        let text_view_state = text_view_state.read(cx);
        let is_selectable = text_view_state.is_selectable();
        if !is_selectable {
            return (false, false, None);
        }

        if text_view_state.is_all_selected() {
            return (is_selectable, true, Some((0..self.text.len()).into()));
        }

        if let Some(selection) = text_view_state.multi_click_selection() {
            return (
                is_selectable,
                true,
                selection_for_multi_click(
                    &self.text,
                    text_layout,
                    *bounds,
                    selection.pos,
                    selection.kind,
                )
                .map(Selection::from),
            );
        }

        let Some((selection_start, selection_end)) = text_view_state.selection_points(window, cx)
        else {
            return (is_selectable, false, None);
        };
        let line_height = window.line_height();
        let mask_bounds = window.content_mask().bounds;

        // Use for debug selection bounds
        // self.paint_selected_bounds(Bounds::from_corners(selection_start, selection_end), window, cx);

        let mut selection: Option<Selection> = None;
        let mut offset = 0;
        let mut chars = self.text.chars().peekable();
        while let Some(c) = chars.next() {
            let next_offset = offset + c.len_utf8();
            let Some((pos, char_width)) = char_cell(text_layout, offset, c, line_height) else {
                offset = next_offset;
                continue;
            };

            let char_center = point(pos.x + char_width.half(), pos.y + line_height.half());
            if mask_bounds.contains(&char_center)
                && point_in_text_selection(
                    pos,
                    char_width,
                    selection_start,
                    selection_end,
                    line_height,
                )
            {
                if selection.is_none() {
                    selection = Some((offset..offset).into());
                }

                if let Some(selection) = selection.as_mut() {
                    selection.end = next_offset;
                }
            }

            offset = next_offset;
        }

        (true, true, selection)
    }

    fn text_line_bounds(
        &self,
        text_layout: &TextLayout,
        line_height: Pixels,
        mask_bounds: Bounds<Pixels>,
    ) -> Vec<Bounds<Pixels>> {
        let mut line_bounds = Vec::new();
        let mut current_line_y = None;
        let mut current_bounds: Option<Bounds<Pixels>> = None;
        let mut offset = 0;

        for c in self.text.chars() {
            let next_offset = offset + c.len_utf8();
            let Some((pos, char_width)) = char_cell(text_layout, offset, c, line_height) else {
                offset = next_offset;
                continue;
            };

            let bounds = Bounds::from_corners(pos, point(pos.x + char_width, pos.y + line_height))
                .intersect(&mask_bounds);
            if bounds.size.width > px(0.) && bounds.size.height > px(0.) {
                if current_line_y == Some(pos.y) {
                    if let Some(current) = current_bounds.as_mut() {
                        *current = current.union(&bounds);
                    }
                } else {
                    if let Some(current) = current_bounds.take() {
                        line_bounds.push(current);
                    }
                    current_line_y = Some(pos.y);
                    current_bounds = Some(bounds);
                }
            }

            offset = next_offset;
        }

        if let Some(current) = current_bounds {
            line_bounds.push(current);
        }

        line_bounds
    }

    /// Paint the selection background.
    fn paint_selection(
        &self,
        selection: &Selection,
        text_layout: &TextLayout,
        bounds: &Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let mut start = selection.start;
        let mut end = selection.end;
        if end < start {
            std::mem::swap(&mut start, &mut end);
        }
        let line_height = text_layout.line_height();
        // The start index is inclusive, so a selection beginning on a wrapped
        // row must be drawn from that row's left edge rather than from the
        // phantom end-of-previous-row position the layout reports for it. The
        // end index is exclusive, and its phantom position is the right place
        // to stop: the selection ends where the previous row ends.
        let start_position = self
            .text
            .get(start..)
            .and_then(|rest| rest.chars().next())
            .and_then(|c| char_cell(text_layout, start, c, line_height))
            .map(|(pos, _)| pos)
            .or_else(|| text_layout.position_for_index(start));
        let Some(start_position) = start_position else {
            return;
        };
        let Some(end_position) = text_layout.position_for_index(end) else {
            return;
        };

        if start_position.y == end_position.y {
            window.paint_quad(quad(
                Bounds::from_corners(
                    start_position,
                    point(end_position.x, end_position.y + line_height),
                ),
                px(0.),
                cx.theme().selection,
                Edges::default(),
                gpui::transparent_black(),
                BorderStyle::default(),
            ));
        } else {
            window.paint_quad(quad(
                Bounds::from_corners(
                    start_position,
                    point(bounds.right(), start_position.y + line_height),
                ),
                px(0.),
                cx.theme().selection,
                Edges::default(),
                gpui::transparent_black(),
                BorderStyle::default(),
            ));

            if end_position.y > start_position.y + line_height {
                window.paint_quad(quad(
                    Bounds::from_corners(
                        point(bounds.left(), start_position.y + line_height),
                        point(bounds.right(), end_position.y),
                    ),
                    px(0.),
                    cx.theme().selection,
                    Edges::default(),
                    gpui::transparent_black(),
                    BorderStyle::default(),
                ));
            }

            window.paint_quad(quad(
                Bounds::from_corners(
                    point(bounds.left(), end_position.y),
                    point(end_position.x, end_position.y + line_height),
                ),
                px(0.),
                cx.theme().selection,
                Edges::default(),
                gpui::transparent_black(),
                BorderStyle::default(),
            ));
        }
    }
}

impl IntoElement for Inline {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Inline {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_element_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let text_style = window.text_style();

        // A link is coloured at rest and ruled only while the pointer is on
        // it, so the rule marks the one target under the cursor instead of
        // every link on the page. It is dashed because the run is already
        // coloured; a solid rule under coloured text reads as two marks for
        // one thing.
        let hovered = self
            .state
            .lock()
            .ok()
            .and_then(|state| state.hovered_link.clone());
        let hovered: Vec<Range<usize>> = hovered
            .map(|url| {
                self.links
                    .iter()
                    .filter(|(_, link)| link.url == url)
                    .map(|(range, _)| range.clone())
                    .collect()
            })
            .unwrap_or_default();

        let mut runs = Vec::new();
        let mut ix = 0;
        for (range, highlight) in self.highlights.iter() {
            if ix < range.start {
                runs.push(text_style.clone().to_run(range.start - ix));
            }

            let mut highlight = *highlight;
            if hovered
                .iter()
                .any(|link| link.start <= range.start && range.end <= link.end)
            {
                highlight.underline = Some(UnderlineStyle {
                    thickness: px(1.),
                    dashed: true,
                    ..Default::default()
                });
            }

            runs.push(text_style.clone().highlight(highlight).to_run(range.len()));
            ix = range.end;
        }
        if ix < self.text.len() {
            runs.push(text_style.to_run(self.text.len() - ix));
        }

        self.styled_text = StyledText::new(self.text.clone()).with_runs(runs);
        let (layout_id, _) =
            self.styled_text
                .request_layout(global_element_id, inspector_id, window, cx);

        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.styled_text
            .prepaint(id, inspector_id, bounds, &mut (), window, cx);

        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        hitbox
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let current_view = window.current_view();
        let hitbox = prepaint;
        let Ok(mut state) = self.state.lock() else {
            return;
        };

        let text_layout = self.styled_text.layout().clone();
        self.styled_text
            .paint(global_id, None, bounds, &mut (), &mut (), window, cx);

        // layout selections
        let (is_selectable, is_selection, selection) =
            self.layout_selections(&text_layout, &bounds, window, cx);

        state.selection = selection;

        if is_selection || is_selectable {
            window.set_cursor_style(CursorStyle::IBeam, &hitbox);
        }

        // link cursor pointer
        let mouse_position = window.mouse_position();
        if let Some(_) = Self::link_for_position(&text_layout, &self.links, mouse_position) {
            window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
        }

        if let Some(selection) = &state.selection {
            self.paint_selection(selection, &text_layout, &bounds, window, cx);
        }

        if is_selectable {
            if let Some(text_view_state) = GlobalState::global(cx).text_view_state().cloned() {
                let text_bounds = self.text_line_bounds(
                    &text_layout,
                    text_layout.line_height(),
                    window.content_mask().bounds,
                );
                crate::Root::register_selectable_text_inline(
                    &text_view_state,
                    text_bounds,
                    window,
                    cx,
                );
            }

            window.on_mouse_event({
                let hitbox = hitbox.clone();
                let text_layout = text_layout.clone();
                let inline_state = self.state.clone();
                let text = self.text.clone();
                let text_view_state = GlobalState::global(cx).text_view_state().cloned();

                move |event: &MouseDownEvent, phase, window, cx| {
                    if !phase.bubble()
                        || !hitbox.is_hovered(window)
                        || event.button != MouseButton::Left
                    {
                        return;
                    }

                    let kind = match event.click_count {
                        2 => TextViewMultiClickKind::Word,
                        3 => TextViewMultiClickKind::Paragraph,
                        _ => return,
                    };

                    let Some(range) = selection_for_multi_click(
                        &text,
                        &text_layout,
                        hitbox.bounds,
                        event.position,
                        kind,
                    ) else {
                        return;
                    };

                    let selected_text = text[range.clone()].to_string();

                    if let Ok(mut inline_state) = inline_state.lock() {
                        inline_state.selection = Some(range.into());
                    }
                    if let Some(text_view_state) = &text_view_state {
                        text_view_state.update(cx, |state, _| {
                            state.set_multi_click_selection(event.position, kind, selected_text);
                        });
                    }
                    cx.notify(current_view);
                }
            });
        }

        window.on_mouse_event({
            let hitbox = hitbox.clone();
            let text_layout = text_layout.clone();
            let links = self.links.clone();
            let inline_state = self.state.clone();

            move |event: &MouseMoveEvent, phase, window, cx| {
                if !phase.bubble() {
                    return;
                }

                let hovered_link = if hitbox.is_hovered(window) {
                    Self::link_for_position(&text_layout, &links, event.position)
                        .map(|link| link.url)
                } else {
                    None
                };
                let previous_link = {
                    let Ok(mut state) = inline_state.lock() else {
                        return;
                    };
                    if state.hovered_link == hovered_link {
                        return;
                    }
                    std::mem::replace(&mut state.hovered_link, hovered_link.clone())
                };

                // The rule under the link is drawn from this state, and a
                // pointer move is outside the frame loop, so the change has to
                // wake the pump itself or it would only show on whatever
                // repaint happened along next.
                cx.notify(current_view);

                let Some(overlay) = crate::Root::tooltip_overlay(window, cx) else {
                    return;
                };
                overlay.update(cx, |overlay, cx| {
                    if let Some(url) = hovered_link {
                        overlay.request_text(url, hitbox.bounds, window, cx);
                    } else if previous_link.is_some() {
                        overlay.request_hide(window, cx);
                    }
                });
            }
        });

        if !is_selection {
            // click to open link
            window.on_mouse_event({
                let links = self.links.clone();
                let text_layout = text_layout.clone();
                let hitbox = hitbox.clone();
                let text_view_state = GlobalState::global(cx).text_view_state().cloned();
                let link_click_handler = self.link_click_handler.clone();

                move |event: &MouseUpEvent, phase, window, cx| {
                    if !phase.bubble() || !hitbox.is_hovered(window) {
                        return;
                    }
                    if text_view_state
                        .as_ref()
                        .is_some_and(|state| state.read(cx).has_selection(window, cx))
                    {
                        return;
                    }

                    if let Some(link) =
                        Self::link_for_position(&text_layout, &links, event.position)
                    {
                        window.end_text_selection(cx);
                        cx.stop_propagation();
                        open_link_target(&link.url, link_click_handler.as_ref(), window, cx);
                    }
                }
            });
        }
    }
}

fn selection_for_multi_click(
    text: &str,
    text_layout: &TextLayout,
    bounds: Bounds<Pixels>,
    pos: Point<Pixels>,
    kind: TextViewMultiClickKind,
) -> Option<std::ops::Range<usize>> {
    if !bounds.contains(&pos) {
        return None;
    }

    let offset = text_layout.index_for_position(pos).ok()?;

    match kind {
        TextViewMultiClickKind::Word => word_range_at(text, offset),
        // Known limitation: a paragraph maps to a single Inline run here. When a
        // paragraph embeds an inline image it is split into multiple Inline runs,
        // so triple-click only selects the run on the clicked side of the image.
        TextViewMultiClickKind::Paragraph => (!text.is_empty()).then_some(0..text.len()),
    }
}

/// Check if a `pos` is within a `bounds`, considering multi-line selections.
fn point_in_text_selection(
    pos: Point<Pixels>,
    char_width: Pixels,
    selection_start: Point<Pixels>,
    selection_end: Point<Pixels>,
    line_height: Pixels,
) -> bool {
    let point_in_line = |point: Point<Pixels>| point.y >= pos.y && point.y < pos.y + line_height;
    let top = selection_start.y.min(selection_end.y);
    let bottom = selection_start.y.max(selection_end.y);
    let x = pos.x + char_width.half();

    // Out of the vertical bounds
    if pos.y + line_height <= top || pos.y > bottom {
        return false;
    }

    // Treat the selection as single-line when both drag points fall within the
    // same rendered line, even if their y coordinates differ inside that line.
    if point_in_line(selection_start) && point_in_line(selection_end) {
        let left = selection_start.x.min(selection_end.x);
        let right = selection_start.x.max(selection_end.x);
        return x >= left && x <= right;
    }

    let (top_point, bottom_point) = if selection_start.y < selection_end.y {
        (selection_start, selection_end)
    } else {
        (selection_end, selection_start)
    };
    let is_top_line = point_in_line(top_point);
    let is_bottom_line = point_in_line(bottom_point);

    if is_top_line {
        return x >= top_point.x;
    } else if is_bottom_line {
        return x <= bottom_point.x;
    } else {
        return true;
    }
}

#[cfg(test)]
mod tests {
    use super::{char_cell_at, point_in_text_selection};
    use gpui::{point, px};

    #[test]
    fn test_char_cell_at() {
        let line_height = px(20.);
        let left = px(10.);
        let pos = point(px(50.), px(0.));

        // Mid-row character: the next position gives the advance width.
        assert_eq!(
            char_cell_at(pos, Some(point(px(58.), px(0.))), left, 'a', line_height),
            (pos, px(8.))
        );

        // First character of a wrapped row: the layout reports the end of the
        // row it wrapped out of, so the cell moves to the next row's left edge.
        assert_eq!(
            char_cell_at(pos, Some(point(px(18.), px(20.))), left, 'a', line_height),
            (point(left, px(20.)), px(8.))
        );

        // A hard line break stays where the layout put it.
        assert_eq!(
            char_cell_at(pos, Some(point(px(10.), px(20.))), left, '\n', line_height),
            (pos, px(10.))
        );

        // Last known character: fall back to half the line height.
        assert_eq!(
            char_cell_at(pos, None, left, 'a', line_height),
            (pos, px(10.))
        );
    }

    #[test]
    fn test_point_in_text_selection() {
        let line_height = px(20.);
        let char_width = px(10.);
        let start = point(px(50.), px(50.));
        let end = point(px(150.), px(150.));

        // First line but haft line height, true
        // | p --------|
        // | selection |
        // |-----------|
        assert!(point_in_text_selection(
            point(px(50.), px(40.)),
            char_width,
            start,
            end,
            line_height
        ));

        // First line in selection, true
        // | p --------|
        // | selection |
        // |-----------|
        assert!(point_in_text_selection(
            point(px(50.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));
        // First line, but left out of selection, false
        // p |-----------|
        //   | selection |
        //   |-----------|
        assert!(!point_in_text_selection(
            point(px(40.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));
        // First line but right out of selection, true
        // |-----------| p
        // | selection |
        // |-----------|
        assert!(point_in_text_selection(
            point(px(160.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));

        // Middle line in selection, true
        // |-----------|
        // |     p     |
        // |-----------|
        assert!(point_in_text_selection(
            point(px(100.), px(70.)),
            char_width,
            start,
            end,
            line_height
        ));
        // Middle line, but left out of selection, true
        //   |-----------|
        // p | selection |
        //   |-----------|
        assert!(point_in_text_selection(
            point(px(40.), px(70.)),
            char_width,
            start,
            end,
            line_height
        ));
        // Middle line, but right out of selection, true
        // |-----------|
        // | selection | p
        // |-----------|
        assert!(point_in_text_selection(
            point(px(160.), px(70.)),
            char_width,
            start,
            end,
            line_height
        ));

        // Last line in selection, true
        // |-----------|
        // | selection |
        // |------- p -|
        assert!(point_in_text_selection(
            point(px(100.), px(140.)),
            char_width,
            start,
            end,
            line_height
        ));
        // Last line, but left out of selection, true
        //
        //   |-----------|
        //   | selection |
        // p |-----------|
        assert!(point_in_text_selection(
            point(px(40.), px(140.)),
            char_width,
            start,
            end,
            line_height
        ));
        // Last line, but right out of selection, false
        // |-----------|
        // | selection |
        // |-----------| p
        assert!(!point_in_text_selection(
            point(px(160.), px(140.)),
            char_width,
            start,
            end,
            line_height
        ));

        // Out of vertical bounds (top), false
        //       p
        // |-----------|
        // | selection |
        // |-----------|
        assert!(!point_in_text_selection(
            point(px(100.), px(20.)),
            char_width,
            start,
            end,
            line_height
        ));
        // Out of vertical bounds (bottom), false
        // |-----------|
        // | selection |
        // |-----------|
        //       p
        assert!(!point_in_text_selection(
            point(px(100.), px(160.)),
            char_width,
            start,
            end,
            line_height
        ));
    }

    #[test]
    fn test_point_in_text_selection_reversed_drag_direction() {
        let line_height = px(20.);
        let char_width = px(10.);

        // Mouse down on lower line then drag upward to x=150.
        // Top line should follow current mouse x, bottom line should keep anchor x.
        let start = point(px(80.), px(150.));
        let end = point(px(150.), px(50.));

        // On top line, selection starts from top cursor x (150), so x=140 should be excluded.
        assert!(!point_in_text_selection(
            point(px(140.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));
        assert!(point_in_text_selection(
            point(px(150.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));

        // On bottom line, selection ends at anchor x (80), so x=90 should be excluded.
        assert!(point_in_text_selection(
            point(px(75.), px(140.)),
            char_width,
            start,
            end,
            line_height
        ));
        assert!(!point_in_text_selection(
            point(px(80.), px(140.)),
            char_width,
            start,
            end,
            line_height
        ));
    }

    #[test]
    fn test_point_in_text_selection_same_visual_line_with_different_y() {
        let line_height = px(20.);
        let char_width = px(10.);
        let start = point(px(100.), px(55.));
        let end = point(px(60.), px(58.));

        assert!(!point_in_text_selection(
            point(px(40.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));
        assert!(point_in_text_selection(
            point(px(70.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));
        assert!(!point_in_text_selection(
            point(px(110.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));
    }

    #[test]
    fn test_point_in_text_selection_same_visual_line_with_reversed_y() {
        let line_height = px(20.);
        let char_width = px(10.);
        let start = point(px(60.), px(58.));
        let end = point(px(100.), px(55.));

        assert!(!point_in_text_selection(
            point(px(40.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));
        assert!(point_in_text_selection(
            point(px(70.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));
        assert!(!point_in_text_selection(
            point(px(110.), px(50.)),
            char_width,
            start,
            end,
            line_height
        ));
    }
}
