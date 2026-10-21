use std::sync::{Arc, Weak};

use gpui::Global;
use gpui_base::text::CodeBlockHighlighterFn;

use crate::highlighter::HighlightTheme;
use crate::text::component_code_block_highlighter;

#[derive(Default)]
pub(super) struct CodeBlockHighlighters {
    entries: Vec<CachedHighlighter>,
}

struct CachedHighlighter {
    theme: Weak<HighlightTheme>,
    callback: Weak<CodeBlockHighlighterFn>,
}

impl Global for CodeBlockHighlighters {}

impl CodeBlockHighlighters {
    pub(super) fn get(&mut self, theme: Arc<HighlightTheme>) -> Arc<CodeBlockHighlighterFn> {
        self.entries
            .retain(|entry| entry.callback.strong_count() > 0);
        for entry in &self.entries {
            if let Some(cached_theme) = entry.theme.upgrade()
                && Arc::ptr_eq(&cached_theme, &theme)
                && let Some(callback) = entry.callback.upgrade()
            {
                return callback;
            }
        }

        let callback: Arc<CodeBlockHighlighterFn> =
            Arc::new(component_code_block_highlighter(theme.clone()));
        self.entries.push(CachedHighlighter {
            theme: Arc::downgrade(&theme),
            callback: Arc::downgrade(&callback),
        });
        callback
    }
}
