use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use gpui::{HighlightStyle, Hsla};
use gpui_component::highlighter::HighlightTheme;

use crate::agent_tab::transcript::code::prepared::PreparedCode;

#[derive(Default)]
pub(super) struct CodeRenderCache {
    source: Option<Arc<PreparedCode>>,
    theme: Option<Arc<HighlightTheme>>,
    foreground: Hsla,
    background: Hsla,
    rows: HashMap<usize, Vec<(Range<usize>, HighlightStyle)>>,
}

impl CodeRenderCache {
    pub(super) fn update(
        &mut self,
        source: &Arc<PreparedCode>,
        theme: &Arc<HighlightTheme>,
        foreground: Hsla,
        background: Hsla,
    ) {
        if self
            .source
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old, source))
            && self
                .theme
                .as_ref()
                .is_some_and(|old| Arc::ptr_eq(old, theme))
            && self.foreground == foreground
            && self.background == background
        {
            return;
        }

        self.source = Some(source.clone());
        self.theme = Some(theme.clone());
        self.foreground = foreground;
        self.background = background;

        self.rows.clear();
    }

    pub(super) fn styles(
        &mut self,
        row: usize,
        range: Range<usize>,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        if let Some(styles) = self.rows.get(&row) {
            return styles.clone();
        }

        // Bound retained rows while scrolling through a large output. Each
        // row contains at most one bounded text segment.
        if self.rows.len() >= 256 {
            self.rows.clear();
        }

        let styles = self.source.as_ref().expect("render source").styles(
            range,
            self.theme.as_ref().expect("render theme"),
            self.foreground,
            self.background,
        );

        self.rows.insert(row, styles.clone());

        styles
    }
}
