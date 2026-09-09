use std::ops::Range;

use gpui::HighlightStyle;

use crate::text::node::CodeBlock;

impl CodeBlock {
    pub(crate) fn set_highlights(&self, highlights: Vec<(Range<usize>, HighlightStyle)>) -> bool {
        let Ok(mut styles) = self.external_highlights.lock() else {
            return false;
        };
        if styles.as_ref() == Some(&highlights) {
            return false;
        }
        *styles = Some(highlights);
        true
    }

    pub(crate) fn external_highlights(&self) -> Option<Vec<(Range<usize>, HighlightStyle)>> {
        self.external_highlights.lock().ok()?.clone()
    }
}
