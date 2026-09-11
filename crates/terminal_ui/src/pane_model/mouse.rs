use nmt_input::keyboard::ModifiersState;
use nmt_terminal::ghostty::BlockHandle;
use nmt_terminal::selection::SelectionType;
use nmt_terminal::session::request::{BlockRange, Request};
use nmt_terminal::session::{
    BlockPoint, SurfaceMouseButton, SurfaceMouseEventKind, SurfaceScreenCell,
};

use crate::block_list::BlockListPoint;
use crate::pane_model::PaneController;
use crate::pane_model::selection_drag::{
    block_gutter_hit, selection_drag_started, selection_type_for_click_count,
};
use crate::pane_model::viewport::LocalPoint;

const BLOCK_GUTTER_SELECTION_ENABLED: bool = false;

pub(crate) struct MouseInput {
    pub position: LocalPoint,
    pub button: Option<SurfaceMouseButton>,
    pub modifiers: ModifiersState,
    pub click_count: usize,
    pub follow_link: bool,
}

pub(crate) enum MouseOutcome {
    Ignored,
    OpenUrl(String),
    SelectionChanged,
    FrozenSelectionStarted,
    EngineHandled,
}

pub(crate) struct PendingExpansion {
    point: BlockPoint,
    handle: BlockHandle,
    request: Request<BlockRange>,
    kind: SelectionType,
}

impl PendingExpansion {
    pub(super) fn copy_text(&self, model: &PaneController) -> Request<String> {
        model.source.session.block_selection_text(
            self.handle,
            self.point.line,
            self.point.col,
            self.kind,
        )
    }
}

impl PaneController {
    pub(crate) fn poll_expansion(&mut self) {
        let Some(mut pending) = self.pending_expansion.take() else {
            return;
        };
        match pending.request.try_recv() {
            Ok(Some(Ok(((start_line, start_col), (end_line, end_col))))) => {
                let current = self
                    .source
                    .session
                    .block_item(pending.point.item)
                    .and_then(|item| item.handle());
                if current.is_some_and(|handle| {
                    handle.id == pending.handle.id && handle.generation == pending.handle.generation
                }) {
                    self.frozen_drag.select(Some((
                        BlockPoint {
                            item: pending.point.item,
                            line: start_line,
                            col: start_col,
                        },
                        BlockPoint {
                            item: pending.point.item,
                            line: end_line,
                            col: end_col,
                        },
                    )));
                }
            }
            Ok(None) => self.pending_expansion = Some(pending),
            _ => {}
        }
    }

    pub(crate) fn mouse_down(&mut self, input: MouseInput) -> MouseOutcome {
        self.selection_generation = self.selection_generation.wrapping_add(1);
        self.pending_expansion = None;
        self.frozen_drag.set_origin(None);
        let left = input.button == Some(SurfaceMouseButton::Left);
        if left
            && input.follow_link
            && let Some(link) = self.link_at_position(input.position)
        {
            return MouseOutcome::OpenUrl(link.url);
        }
        let mut cleared = false;
        if BLOCK_GUTTER_SELECTION_ENABLED
            && left
            && self.block_list_mode()
            && self.settings.show_block_chrome
            && !self.source.session.mouse_reporting_active()
        {
            if block_gutter_hit(input.position.x, 0.0)
                && let Some(item) = self.frozen.item_at(input.position.y)
            {
                self.gutter.select(item);
                return MouseOutcome::SelectionChanged;
            }
            cleared = self.gutter.clear_selection();
        }
        let reports = self
            .source
            .session
            .mouse_reporting_active_for(input.modifiers);
        let kind = selection_type_for_click_count(input.click_count);
        self.frozen_drag
            .set_origin((left && !reports).then_some(input.position));
        if self.block_list_mode() && !reports {
            if left
                && let Some(BlockListPoint::Frozen(point)) =
                    self.block_list_point_at(input.position)
            {
                self.source.session.clear_selection();
                if kind == SelectionType::Simple {
                    self.frozen_drag.begin(point);
                } else {
                    self.frozen_drag.clear();
                    if let Some(handle) = self
                        .source
                        .session
                        .block_item(point.item)
                        .and_then(|item| item.handle())
                        && let Some(request) =
                            self.source.session.expand_frozen_selection(point, kind)
                    {
                        self.pending_expansion = Some(PendingExpansion {
                            kind,
                            point,
                            handle,
                            request,
                        });
                    }
                }
                return MouseOutcome::FrozenSelectionStarted;
            }
            cleared |= self.frozen_drag.clear();
        }
        match self.apply_mouse(input, SurfaceMouseEventKind::Down, kind) {
            MouseOutcome::Ignored if cleared => MouseOutcome::SelectionChanged,
            outcome => outcome,
        }
    }

    pub(crate) fn mouse_up(&mut self, input: MouseInput) -> MouseOutcome {
        self.frozen_drag.set_origin(None);
        if self.frozen_drag.commit() {
            return MouseOutcome::Ignored;
        }
        self.apply_mouse(input, SurfaceMouseEventKind::Up, SelectionType::Simple)
    }

    pub(crate) fn mouse_move(&mut self, input: MouseInput) -> MouseOutcome {
        if let Some(origin) = self.frozen_drag.origin() {
            let Some(cell) = self.cell_metrics else {
                return MouseOutcome::Ignored;
            };
            if !selection_drag_started(origin, input.position, cell.width_px) {
                return MouseOutcome::Ignored;
            }
            self.frozen_drag.set_origin(None);
        }
        if self.frozen_drag.anchor().is_some() {
            let mut position = input.position;
            position.y = position.y.min((self.frozen.active_top() - 1.0).max(0.0));
            if let Some(BlockListPoint::Frozen(head)) = self.block_list_point_at(position)
                && self.frozen_drag.extend(head)
            {
                return MouseOutcome::SelectionChanged;
            }
            return MouseOutcome::Ignored;
        }
        self.apply_mouse(input, SurfaceMouseEventKind::Move, SelectionType::Simple)
    }

    fn apply_mouse(
        &self,
        input: MouseInput,
        kind: SurfaceMouseEventKind,
        selection: SelectionType,
    ) -> MouseOutcome {
        let Some(metrics) = self.cell_metrics else {
            return MouseOutcome::Ignored;
        };
        let (cell, side) = self.viewport.cell_at(input.position, metrics);
        let handled = if self.block_list_mode()
            && !self
                .source
                .session
                .mouse_reporting_active_for(input.modifiers)
            && input.button == Some(SurfaceMouseButton::Left)
            && let Some(point) = self.block_list_point_at(input.position)
        {
            let screen = match point {
                BlockListPoint::LiveHistory { row, col } => SurfaceScreenCell { row, col },
                BlockListPoint::Frozen(point) => SurfaceScreenCell {
                    row: 0,
                    col: point.col.min(u16::MAX as u32) as u16,
                },
            };
            self.source
                .session
                .apply_screen_selection(screen, side, kind, selection)
        } else if input.button == Some(SurfaceMouseButton::Left)
            && !self
                .source
                .session
                .mouse_reporting_active_for(input.modifiers)
        {
            self.source.session.apply_screen_selection(
                SurfaceScreenCell {
                    col: cell.col,
                    row: self
                        .source
                        .snapshot
                        .viewport_top
                        .unwrap_or(0)
                        .saturating_add(u32::from(cell.row)),
                },
                side,
                kind,
                selection,
            )
        } else {
            self.source.session.apply_mouse(
                cell,
                side,
                input.button,
                kind,
                input.modifiers,
                selection,
            )
        };
        if handled {
            MouseOutcome::EngineHandled
        } else {
            MouseOutcome::Ignored
        }
    }

    pub(crate) fn scroll_wheel(
        &self,
        position: LocalPoint,
        lines: i32,
        modifiers: ModifiersState,
    ) -> bool {
        if lines == 0 || (self.block_list_mode() && !self.source.session.mouse_reporting_active()) {
            return false;
        }
        let Some(metrics) = self.cell_metrics else {
            return false;
        };
        let (cell, _) = self.viewport.cell_at(position, metrics);
        self.source.session.apply_scroll(cell, lines, modifiers)
    }
}
