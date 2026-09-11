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
