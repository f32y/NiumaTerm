use nmt_terminal::selection::SelectionType;

use crate::block_list::FrozenPoint;
use crate::pane_model::viewport::LocalPoint;
use crate::theme::{BLOCK_GUTTER_GAP, BLOCK_GUTTER_WIDTH};

/// A pointer x hits the block gutter when it falls in the strip painted in the
/// left padding, with a small tolerance into column 0.
pub(crate) fn block_gutter_hit(x: f32, origin_x: f32) -> bool {
    let left = origin_x - BLOCK_GUTTER_GAP - BLOCK_GUTTER_WIDTH - 2.0;
    let right = origin_x + 3.0;
    (left..=right).contains(&x)
}

pub(crate) fn selection_drag_started(
    origin: LocalPoint,
    position: LocalPoint,
    cell_width: f32,
) -> bool {
    let dx = position.x - origin.x;
    let dy = position.y - origin.y;

    dx * dx + dy * dy >= cell_width * cell_width / 16.0
}

pub(crate) fn selection_type_for_click_count(click_count: usize) -> SelectionType {
    match click_count {
        2 => SelectionType::Semantic,
        3.. => SelectionType::Lines,
        _ => SelectionType::Simple,
    }
}

/// A selection gesture in the frozen block region.
///
/// The three values are one gesture at three stages: the pixel the press
/// landed on, the cell it resolved to once the pointer moved far enough, and
/// the range that grew from it. A press alone selects nothing, so the anchor
/// exists only between the press and the first move that commits to a drag.
#[derive(Default)]
pub(crate) struct FrozenSelectionDrag {
    /// Frozen-region selection: (anchor, head), both inclusive cell points.
    selection: Option<(FrozenPoint, FrozenPoint)>,
    /// Anchor of an in-progress frozen-region drag. The selection itself is
    /// only created on the first mouse-move, so a plain click selects nothing
    /// (matching the engine's empty-selection-dropped-on-up semantics).
    anchor: Option<FrozenPoint>,
    /// Pixel origin of a text-selection gesture. Ignoring movement within a
    /// quarter-cell radius prevents normal hand jitter from selecting a glyph.
    drag_origin: Option<LocalPoint>,
}

impl FrozenSelectionDrag {
    pub(crate) fn origin(&self) -> Option<LocalPoint> {
        self.drag_origin
    }

    pub(crate) fn set_origin(&mut self, origin: Option<LocalPoint>) {
        self.drag_origin = origin;
    }

    /// Start a drag from `anchor`, dropping whatever was selected.
    pub(crate) fn begin(&mut self, anchor: FrozenPoint) {
        self.selection = None;
        self.anchor = Some(anchor);
    }

    /// Take a range whole, as a double- or triple-click does. There is nothing
    /// left to drag from, so the anchor goes with it.
    pub(crate) fn select(&mut self, selection: Option<(FrozenPoint, FrozenPoint)>) {
        self.selection = selection;
        self.anchor = None;
    }

    pub(crate) fn anchor(&self) -> Option<FrozenPoint> {
        self.anchor
    }

    /// Grow the selection to `head`, reporting whether a drag was in progress.
    pub(crate) fn extend(&mut self, head: FrozenPoint) -> bool {
        let Some(anchor) = self.anchor else {
            return false;
        };

        self.selection = Some((anchor, head));

        true
    }

    /// End a drag without committing it, reporting whether one was open.
    pub(crate) fn commit(&mut self) -> bool {
        self.anchor.take().is_some()
    }

    /// Drop the selection, reporting whether one was showing.
    pub(crate) fn clear(&mut self) -> bool {
        self.selection.take().is_some()
    }

    pub(crate) fn current(&self) -> Option<(FrozenPoint, FrozenPoint)> {
        self.selection
    }
}
