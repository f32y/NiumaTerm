use nmt_input::keyboard::ModifiersState;
use nmt_terminal::input::WheelDelta;
use nmt_terminal::links::follows_link;
use nmt_terminal::selection::SelectionType;
use nmt_terminal::session::interaction::selection_type_for_click_count;
use nmt_terminal::session::{SurfaceMouseButton, SurfaceMouseEventKind, SurfaceScreenCell};

use crate::block_list::BlockListPoint;
use crate::pane_model::PaneController;
use crate::pane_model::scroll::ScrollOutcome;
use crate::pane_model::selection_geometry::{block_gutter_hit, selection_drag_started};
use crate::pane_model::viewport::LocalPoint;

const BLOCK_GUTTER_SELECTION_ENABLED: bool = false;

pub(crate) struct MouseInput {
    pub position: LocalPoint,
    pub button: Option<SurfaceMouseButton>,
    pub modifiers: ModifiersState,
    pub click_count: usize,
}

pub(crate) enum MouseOutcome {
    Ignored,
    OpenUrl(String),
    SelectionChanged,
    FrozenSelectionStarted,
    EngineHandled,
    Scrolled(ScrollOutcome),
    HoverChanged,
}

pub(crate) struct MouseRelease {
    pub outcome: MouseOutcome,
    pub scrollbar_released: bool,
}

pub(crate) struct WheelOutcome {
    pub handled: bool,
    pub hover_changed: bool,
}

impl PaneController {
    pub(crate) fn mouse_down(&mut self, input: MouseInput) -> MouseOutcome {
        self.interaction.begin_pointer();
        self.selection_origin = None;
        let left = input.button == Some(SurfaceMouseButton::Left);
        if left
            && follows_link(input.modifiers)
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
        self.selection_origin = (left && !reports).then_some(input.position);
        if self.block_list_mode() && !reports {
            if left
                && let Some(BlockListPoint::Frozen(point)) =
                    self.block_list_point_at(input.position)
            {
                self.interaction
                    .select_block(&self.source.session, point, kind);
                return MouseOutcome::FrozenSelectionStarted;
            }
            cleared |= self.interaction.clear_block_selection();
        }
        match self.apply_mouse(input, SurfaceMouseEventKind::Down, kind) {
            MouseOutcome::Ignored if cleared => MouseOutcome::SelectionChanged,
            outcome => outcome,
        }
    }

    pub(crate) fn mouse_up(&mut self, input: MouseInput) -> MouseRelease {
        let scrollbar_released = self.scrollbar.end_drag();
        self.selection_origin = None;
        let outcome = if self.interaction.commit_block_selection() {
            MouseOutcome::Ignored
        } else {
            self.apply_mouse(input, SurfaceMouseEventKind::Up, SelectionType::Simple)
        };
        MouseRelease {
            outcome,
            scrollbar_released,
        }
    }

    pub(crate) fn mouse_move(&mut self, input: MouseInput) -> MouseOutcome {
        let hover_changed = if input.button.is_none() {
            self.hover_at(input.position, input.modifiers)
        } else {
            self.links.record_position(input.position);
            false
        };
        match self.move_selection_or_scroll(input) {
            MouseOutcome::Ignored | MouseOutcome::Scrolled(ScrollOutcome::Ignored)
                if hover_changed =>
            {
                MouseOutcome::HoverChanged
            }
            outcome => outcome,
        }
    }

    fn move_selection_or_scroll(&mut self, input: MouseInput) -> MouseOutcome {
        if self.scrollbar.is_dragging() {
            let fraction = (input.position.y / self.content_size.1.max(1.0)).clamp(0.0, 1.0);
            return MouseOutcome::Scrolled(
                self.scroll_thumb_to(self.scrollbar.thumb_top_for(fraction)),
            );
        }
        if let Some(origin) = self.selection_origin {
            let Some(cell) = self.cell_metrics else {
                return MouseOutcome::Ignored;
            };
            if !selection_drag_started(origin, input.position, cell.width_px) {
                return MouseOutcome::Ignored;
            }
            self.selection_origin = None;
        }
        if self.interaction.block_anchor().is_some() {
            let mut position = input.position;
            position.y = position.y.min((self.frozen.active_top() - 1.0).max(0.0));
            if let Some(BlockListPoint::Frozen(head)) = self.block_list_point_at(position)
                && self.interaction.extend_block_selection(head)
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
        &mut self,
        position: LocalPoint,
        delta: WheelDelta,
        modifiers: ModifiersState,
    ) -> WheelOutcome {
        let lines = delta.lines();
        let hover_changed = self.links.clear();
        let mut outcome = WheelOutcome {
            handled: false,
            hover_changed,
        };
        if lines == 0 || (self.block_list_mode() && !self.source.session.mouse_reporting_active()) {
            return outcome;
        }
        let Some(metrics) = self.cell_metrics else {
            return outcome;
        };
        let (cell, _) = self.viewport.cell_at(position, metrics);
        outcome.handled = self.source.session.apply_scroll(cell, lines, modifiers);
        outcome
    }
}
