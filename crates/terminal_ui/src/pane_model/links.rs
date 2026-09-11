use nmt_input::keyboard::ModifiersState;
use nmt_terminal::links::{follows_link, resolve_link};

use crate::block_list::BlockListPoint;
use crate::pane_model::PaneController;
use crate::pane_model::viewport::{LocalPoint, LocalRect};

/// A link resolved under the pointer: the URL plus underline rects relative
/// to the content origin (only the visible rows of a wrapped URL get rects).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LinkHit {
    pub(crate) url: String,
    pub(crate) rects: Vec<LocalRect>,
}

/// The Ctrl-hover link underline and the pointer position it was resolved
/// at. The two travel together because pressing or releasing Ctrl without
/// moving the mouse still has to rescan, and that rescan has no event position
/// of its own to work from.
#[derive(Default)]
pub(super) struct LinkHover {
    hit: Option<LinkHit>,
    pub(super) enabled: bool,
    last_position: Option<LocalPoint>,
}

impl LinkHover {
    /// Record the pointer position and the link resolved under it. Returns
    /// whether the underline changed, so the caller repaints only when it did.
    pub(super) fn update(&mut self, position: LocalPoint, hit: Option<LinkHit>) -> bool {
        self.last_position = Some(position);

        if self.hit == hit {
            return false;
        }

        self.hit = hit;

        true
    }

    /// Record where the pointer is without rescanning, so a later modifier
    /// change resolves the link where the pointer actually sits.
    pub(super) fn record_position(&mut self, position: LocalPoint) {
        self.last_position = Some(position);
    }

    pub(super) fn position(&self) -> Option<LocalPoint> {
        self.last_position
    }

    /// Forget where the pointer was, so a modifier change after the pointer
    /// left the pane cannot resurrect an underline.
    pub(super) fn forget_position(&mut self) {
        self.last_position = None;
    }

    /// Drop the underline, reporting whether one was showing.
    pub(super) fn clear(&mut self) -> bool {
        self.hit.take().is_some()
    }

    pub(super) fn current(&self) -> Option<&LinkHit> {
        self.hit.as_ref()
    }
}

impl PaneController {
    pub(crate) fn hovered_link(&self) -> Option<&LinkHit> {
        self.links.current()
    }

    pub(crate) fn pointer_left(&mut self) -> bool {
        self.links.enabled = false;
        self.links.forget_position();
        self.links.clear()
    }

    pub(crate) fn hover_modifiers_changed(&mut self, modifiers: ModifiersState) -> bool {
        self.links.enabled = follows_link(modifiers);
        let Some(position) = self.links.position() else {
            return false;
        };
        let hit = self
            .links
            .enabled
            .then(|| self.link_at_position(position))
            .flatten();
        self.links.update(position, hit)
    }

    pub(super) fn hover_at(&mut self, position: LocalPoint, modifiers: ModifiersState) -> bool {
        if position.x < 0.0
            || position.y < 0.0
            || position.x > self.content_size.0
            || position.y > self.content_size.1
        {
            return self.pointer_left();
        }
        self.links.record_position(position);
        self.hover_modifiers_changed(modifiers)
    }

    /// Resolve the link under a pointer position: the row's OSC 8 span if one
    /// covers the pointed-at cell, else a URL-shaped token in the row text.
    /// Soft-wrapped neighbor rows are joined so long URLs match whole. Also
    /// yields underline rects (content-origin-relative) for hover feedback.
    pub(crate) fn link_at_position(&self, position: LocalPoint) -> Option<LinkHit> {
        let cell_metrics = self.cell_metrics?;

        enum RowSource {
            Screen(i64),
            Block { item: usize, line: i64 },
        }

        let viewport_top = self.source.snapshot.viewport_top;

        let (source, col) = match self.block_list_point_at(position) {
            Some(BlockListPoint::Frozen(pt)) => (
                RowSource::Block {
                    item: pt.item,
                    line: pt.line as i64,
                },
                pt.col as usize,
            ),
            Some(BlockListPoint::LiveHistory { row, col }) => {
                (RowSource::Screen(row as i64), col as usize)
            }
            None => {
                let (cell, _) = self.viewport.cell_at(position, cell_metrics);

                (
                    RowSource::Screen(viewport_top? as i64 + cell.row as i64),
                    cell.col as usize,
                )
            }
        };

        let row_at = |delta: i64| match source {
            RowSource::Screen(row) => u32::try_from(row + delta).ok().and_then(|row| {
                self.source
                    .session
                    .screen_row_text_in(&self.source.snapshot, row)
            }),
            RowSource::Block { item, line, .. } => usize::try_from(line + delta)
                .ok()
                .and_then(|line| self.source.session.block_row_text(item, line)),
        };

        // Content-local y of the row `delta` rows below the pointed-at one;
        // `None` when it is scrolled out of view (that segment gets no rect).
        let row_y = |delta: i64| -> Option<f32> {
            match source {
                RowSource::Screen(row) => {
                    let row = row + delta;
                    let top = viewport_top? as i64;

                    if row < top {
                        // A live-history row above the engine viewport.
                        return self.frozen.row_top(usize::MAX, usize::try_from(row).ok()?);
                    }

                    Some(self.viewport.cursor_y(
                        (row - top).min(u16::MAX as i64) as u16,
                        cell_metrics.height_px,
                    ))
                }
                RowSource::Block { item, line, .. } => self
                    .frozen
                    .row_top(item, usize::try_from(line + delta).ok()?),
            }
        };

        let resolved = resolve_link(col, row_at)?;
        let rects = resolved
            .segments
            .into_iter()
            .filter_map(|segment| {
                let y = row_y(segment.delta)?;
                Some(LocalRect {
                    origin: LocalPoint {
                        x: segment.col as f32 * cell_metrics.width_px,
                        y: y + cell_metrics.height_px - 1.5,
                    },
                    width: segment.cols as f32 * cell_metrics.width_px,
                    height: 1.0,
                })
            })
            .collect();
        Some(LinkHit {
            url: resolved.url,
            rects,
        })
    }
}
