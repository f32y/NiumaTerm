use std::{mem, time};

use crate::terminal_tab::scrollbar::geometry::scrollbar_opacity;

/// Scrollbar drag and fade state. The bar is opaque while the thumb is held
/// and for a linger window after the last scroll, then fades out, so the drag
/// flag and the activity clock have to be read together to paint a frame.
#[derive(Default)]
pub(crate) struct ScrollbarActivity {
    /// True while the scrollbar thumb is being dragged (mouse-move then scrolls
    /// to the pointer instead of selecting text).
    dragging: bool,

    /// Pointer offset inside the thumb at drag start (track fraction), so
    /// grabbing the thumb does not jump it.
    grab: f32,

    /// Last user scroll action; the scrollbar stays opaque within
    /// [`SCROLLBAR_AUTO_HIDE_DELAY`], then fades out unless it is being dragged.
    last_activity: Option<time::Instant>,

    /// Bumped per scroll action so only the newest hide-timer repaints.
    activity_gen: u64,
}

impl ScrollbarActivity {
    pub(crate) fn mark_activity(&mut self) -> u64 {
        self.last_activity = Some(time::Instant::now());
        self.activity_gen = self.activity_gen.wrapping_add(1);

        self.activity_gen
    }

    pub(crate) fn should_fade(&self, generation: u64) -> bool {
        self.activity_gen == generation && !self.dragging
    }

    /// Start a thumb drag, remembering where inside the thumb the pointer
    /// landed as a track fraction.
    pub(crate) fn begin_drag(&mut self, grab: f32) {
        self.dragging = true;
        self.grab = grab;
    }

    pub(super) fn end_drag(&mut self) -> bool {
        mem::take(&mut self.dragging)
    }

    pub(crate) fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// Where the thumb top belongs for a pointer at `fraction` of the track,
    /// so the grabbed point stays under the pointer for the whole drag.
    pub(crate) fn thumb_top_for(&self, fraction: f32) -> f32 {
        fraction - self.grab
    }

    /// Opacity for this frame, or `None` once the bar has faded out completely
    /// and should not be painted at all.
    pub(crate) fn opacity(&self) -> Option<f32> {
        scrollbar_opacity(self.dragging, self.last_activity.map(|at| at.elapsed()))
    }
}
