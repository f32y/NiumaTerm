use crate::input as terminal_input;
use crate::view::*;

const BLOCK_GUTTER_SELECTION_ENABLED: bool = false;

/// A pointer x hits the block gutter when it falls in the strip painted in the
/// left padding, with a small tolerance into column 0.
pub(super) fn block_gutter_hit(x: f32, origin_x: f32) -> bool {
    let left = origin_x - BLOCK_GUTTER_GAP - BLOCK_GUTTER_WIDTH - 2.0;
    let right = origin_x + 3.0;
    (left..=right).contains(&x)
}

pub(super) fn selection_drag_started(
    origin: Point<Pixels>,
    position: Point<Pixels>,
    cell_width: f32,
) -> bool {
    let dx = position.x.as_f32() - origin.x.as_f32();
    let dy = position.y.as_f32() - origin.y.as_f32();

    dx * dx + dy * dy >= cell_width * cell_width / 16.0
}

pub(super) fn selection_type_for_click_count(click_count: usize) -> SelectionType {
    match click_count {
        2 => SelectionType::Semantic,
        3.. => SelectionType::Lines,
        _ => SelectionType::Simple,
    }
}

/// Map a pointer position to a grid cell. `offsets` shifts rows for bottom anchoring.
pub(crate) fn terminal_cell_at_position(
    position: Point<Pixels>,
    origin: Point<Pixels>,
    cell: metrics::CellMetrics,
    offsets: &[f32],
) -> (SurfaceCell, SurfaceCellSide) {
    let x = (position.x.as_f32() - origin.x.as_f32()).max(0.0);
    let y = (position.y.as_f32() - origin.y.as_f32()).max(0.0);
    let col = (x / cell.width_px).floor() as u16;
    let row = terminal_row_at_y(y, cell.height_px, offsets);
    let cell_x = x - (col as f32 * cell.width_px);

    let side = if cell_x < cell.width_px / 2.0 {
        SurfaceCellSide::Left
    } else {
        SurfaceCellSide::Right
    };

    (SurfaceCell { col, row }, side)
}

fn surface_mouse_button(button: MouseButton) -> Option<SurfaceMouseButton> {
    match button {
        MouseButton::Left => Some(SurfaceMouseButton::Left),
        MouseButton::Middle => Some(SurfaceMouseButton::Middle),
        MouseButton::Right => Some(SurfaceMouseButton::Right),
        MouseButton::Navigate(_) => None,
    }
}

const WHEEL_LINES_PER_STEP: f32 = 3.0;

pub(super) fn terminal_scroll_lines(delta: ScrollDelta, cell: metrics::CellMetrics) -> i32 {
    let raw = match delta {
        ScrollDelta::Lines(point) => point.y * WHEEL_LINES_PER_STEP,
        ScrollDelta::Pixels(point) => point.y.as_f32() / cell.height_px.max(1.0),
    };

    if raw.abs() < 0.5 {
        0
    } else {
        raw.round() as i32
    }
}

impl TerminalPane {
    pub(super) fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);

        self.selection_drag_origin = None;

        // Ctrl+left-click opens the URL under the pointer (OSC 8 target or
        // URL-shaped text). It wins over selection and mouse reporting so
        // links stay clickable inside TUIs, matching common terminal behavior.
        if event.button == MouseButton::Left
            && event.modifiers.control
            && !event.modifiers.alt
            && !event.modifiers.shift
            && let Some(link) = self.link_at_position(event.position, cx)
        {
            info!(url = link.url, "ctrl+click open url");
            cx.open_url(&link.url);
            return;
        }

        if BLOCK_GUTTER_SELECTION_ENABLED
            && event.button == MouseButton::Left
            && self.block_chrome_enabled(cx)
            && self.try_select_frozen_item(event.position, cx)
        {
            return;
        }

        // Block-split: a left press in the frozen region starts a frozen
        // selection (and drops the engine one); any other press clears it.
        let reports_mouse = self
            .surface
            .mouse_reporting_active_for(terminal_input::modifiers_state(event.modifiers));

        let selection_type = selection_type_for_click_count(event.click_count);

        self.selection_drag_origin =
            (event.button == MouseButton::Left && !reports_mouse).then_some(event.position);

        if self.block_list_mode(cx) && !reports_mouse {
            if event.button == MouseButton::Left
                && let Some(BlockListPoint::Frozen(pt)) =
                    self.block_list_point_at(event.position, cx)
            {
                // The engine highlight is baked into the cached frame, so
                // clearing the selection needs a frame rebuild too.
                self.surface.clear_selection();

                if selection_type == SelectionType::Simple {
                    self.frozen_selection = None;
                    self.frozen_select_anchor = Some(pt);
                } else {
                    self.frozen_selection = self.expanded_frozen_selection(pt, selection_type);
                    self.frozen_select_anchor = None;
                }

                self.invalidate(cx);

                cx.notify();

                return;
            }

            if self.frozen_selection.take().is_some() {
                cx.notify();
            }
        }

        self.apply_mouse_event(
            event.position,
            Some(event.button),
            SurfaceMouseEventKind::Down,
            event.modifiers,
            selection_type,
            window,
            cx,
        );
    }

    pub(super) fn on_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selection_drag_origin = None;

        if self.scrollbar.is_dragging() {
            // Drag ended: start the linger countdown that hides the bar.
            self.scrollbar.mark_activity(cx);
        }

        self.scrollbar.end_drag();

        if self.frozen_select_anchor.take().is_some() {
            return;
        }

        self.apply_mouse_event(
            event.position,
            Some(event.button),
            SurfaceMouseEventKind::Up,
            event.modifiers,
            SelectionType::Simple,
            window,
            cx,
        );
    }

    pub(crate) fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.links.record_position(event.position);

        if event.pressed_button.is_none() {
            self.update_hovered_link(event.position, event.modifiers, cx);
        }

        if self.scrollbar.is_dragging() {
            let fraction = self.scrollbar_fraction(event.position.y);
            self.scroll_thumb_to(self.scrollbar.thumb_top_for(fraction), cx);
            return;
        }

        if let Some(origin) = self.selection_drag_origin {
            let cell_width = self.cell_metrics(window, cx).width_px;

            if !selection_drag_started(origin, event.position, cell_width) {
                return;
            }

            self.selection_drag_origin = None;
        }

        if let Some(anchor) = self.frozen_select_anchor {
            // Clamp into the frozen region so a drag past the boundary sticks to
            // the last frozen row instead of vanishing.
            let mut pos = event.position;

            let origin = self.content_origin();
            let max_y = origin.y + px((self.frozen_hit.active_top - 1.0).max(0.0));

            if pos.y > max_y {
                pos.y = max_y;
            }

            if let Some(BlockListPoint::Frozen(head)) = self.block_list_point_at(pos, cx) {
                self.frozen_selection = Some((anchor, head));

                cx.notify();
            }

            return;
        }

        self.apply_mouse_event(
            event.position,
            event.pressed_button,
            SurfaceMouseEventKind::Move,
            event.modifiers,
            SelectionType::Simple,
            window,
            cx,
        );
    }

    pub(super) fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Rows shift under the pointer; drop the underline instead of leaving
        // it stale. The next mouse move recomputes it.
        if self.links.clear() {
            cx.notify();
        }

        let cell_metrics = self.cell_metrics(window, cx);

        let lines = terminal_scroll_lines(event.delta, cell_metrics);

        if lines == 0 {
            return;
        }

        // Block-split: scrolling is list state; the engine viewport stays
        // pinned. TUI mouse reporting still goes to the program.
        if self.block_list_mode(cx) && !self.surface.mouse_reporting_active() {
            return;
        }

        let offsets = self.current_row_offsets(cx);

        let (cell, _) = terminal_cell_at_position(
            event.position,
            self.content_origin(),
            cell_metrics,
            &offsets,
        );

        if self.surface.apply_scroll(
            cell,
            lines,
            terminal_input::modifiers_state(event.modifiers),
        ) {
            self.scrollbar.mark_activity(cx);

            self.invalidate(cx);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_mouse_event(
        &mut self,
        position: Point<Pixels>,
        button: Option<MouseButton>,
        kind: SurfaceMouseEventKind,
        modifiers: Modifiers,
        selection_type: SelectionType,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cell_metrics = self.cell_metrics(window, cx);

        let modifiers = terminal_input::modifiers_state(modifiers);

        if self.block_list_mode(cx)
            && !self.surface.mouse_reporting_active_for(modifiers)
            && button == Some(MouseButton::Left)
            && let Some(point) = self.block_list_point_at(position, cx)
        {
            let (_, side) =
                terminal_cell_at_position(position, self.content_origin(), cell_metrics, &[]);

            let cell = match point {
                BlockListPoint::LiveHistory { row, col } => SurfaceScreenCell { row, col },
                // An engine selection cannot cross into an immutable finished
                // block, so dragging above the active block clamps to its first
                // SCREEN row.
                BlockListPoint::Frozen(point) => SurfaceScreenCell {
                    row: 0,
                    col: point.col.min(u16::MAX as u32) as u16,
                },
            };

            if self
                .surface
                .apply_screen_selection(cell, side, kind, selection_type)
            {
                self.invalidate(cx);
            }

            return;
        }

        let offsets = self.current_row_offsets(cx);

        // Block-split: the live grid starts at `active_top` in the list, so
        // shift the mapping origin.
        let mut origin = self.content_origin();

        if self.block_list_mode(cx) {
            origin.y += px(self.frozen_hit.active_top);
        }

        let (cell, side) = terminal_cell_at_position(position, origin, cell_metrics, &offsets);

        let handled = self.surface.apply_mouse(
            cell,
            side,
            button.and_then(surface_mouse_button),
            kind,
            modifiers,
            selection_type,
        );

        if handled {
            self.invalidate(cx);
        }
    }
}
