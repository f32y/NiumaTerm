use nmt_terminal::ghostty::ScrollbarInfo;
use nmt_terminal::session::{SurfaceCell, SurfaceCellSide};

use crate::layout::{row_y_offset, terminal_row_at_y};
use crate::metrics::CellMetrics;
use crate::scrollbar::geometry::scrollbar_offset_for_thumb;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct LocalPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LocalRect {
    pub origin: LocalPoint,
    pub width: f32,
    pub height: f32,
}

pub(crate) enum Viewport {
    Grid {
        scrollbar: ScrollbarInfo,
        row_offsets: Vec<f32>,
    },
    BlockList {
        scroll_px: f32,
        max_scroll_px: f32,
        active_top: f32,
        viewport_px: f32,
    },
}

impl Default for Viewport {
    fn default() -> Self {
        Self::Grid {
            scrollbar: ScrollbarInfo::default(),
            row_offsets: Vec::new(),
        }
    }
}

impl Viewport {
    pub(crate) fn is_scrolled(&self) -> bool {
        match self {
            Self::Grid { scrollbar, .. } => {
                scrollbar.offset < scrollbar.total.saturating_sub(scrollbar.len)
            }
            Self::BlockList {
                scroll_px,
                max_scroll_px,
                ..
            } => scroll_px < max_scroll_px,
        }
    }

    pub(crate) fn scrollbar_info(&self) -> ScrollbarInfo {
        match self {
            Self::Grid { scrollbar, .. } => *scrollbar,
            Self::BlockList {
                scroll_px,
                max_scroll_px,
                viewport_px,
                ..
            } => ScrollbarInfo {
                total: (max_scroll_px + viewport_px).max(0.0) as u64,
                offset: scroll_px.max(0.0) as u64,
                len: viewport_px.max(0.0) as u64,
            },
        }
    }

    pub(crate) fn thumb_target(&self, thumb_top: f32) -> Option<f64> {
        match self {
            Self::Grid { scrollbar, .. } => {
                scrollbar_offset_for_thumb(scrollbar.total as f64, scrollbar.len as f64, thumb_top)
            }
            Self::BlockList {
                max_scroll_px,
                viewport_px,
                ..
            } => scrollbar_offset_for_thumb(
                (max_scroll_px + viewport_px) as f64,
                *viewport_px as f64,
                thumb_top,
            ),
        }
    }

    pub(crate) fn cell_at(
        &self,
        local: LocalPoint,
        cell: CellMetrics,
    ) -> (SurfaceCell, SurfaceCellSide) {
        let x = local.x.max(0.0);
        let (y, offsets) = match self {
            Self::Grid { row_offsets, .. } => (local.y.max(0.0), row_offsets.as_slice()),
            Self::BlockList { active_top, .. } => ((local.y - active_top).max(0.0), &[][..]),
        };
        let col = (x / cell.width_px).floor() as u16;
        let row = terminal_row_at_y(y, cell.height_px, offsets);
        let side = if x - col as f32 * cell.width_px < cell.width_px / 2.0 {
            SurfaceCellSide::Left
        } else {
            SurfaceCellSide::Right
        };
        (SurfaceCell { col, row }, side)
    }

    pub(crate) fn cursor_y(&self, row: u16, cell_h: f32) -> f32 {
        let offset = match self {
            Self::Grid { row_offsets, .. } => row_y_offset(row_offsets, row as usize),
            Self::BlockList { active_top, .. } => *active_top,
        };
        row as f32 * cell_h + offset
    }
}
