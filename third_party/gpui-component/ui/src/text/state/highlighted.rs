use std::ops::Range;

use gpui::{Context, HighlightStyle, SharedString};

use crate::ActiveTheme as _;
use crate::text::document::ParsedDocument;
use crate::text::node::{BlockNode, CodeBlock, Span};
use crate::text::state::{ParsedContent, TextViewState};

impl TextViewState {
    /// Display literal code with externally computed byte-range styles.
    /// Updating only its colors keeps the existing text selection intact.
    pub fn set_highlighted_code(
        &mut self,
        text: SharedString,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
        cx: &mut Context<Self>,
    ) {
        let highlights = highlights
            .into_iter()
            .filter(|(range, _)| {
                range.start < range.end
                    && range.end <= text.len()
                    && text.is_char_boundary(range.start)
                    && text.is_char_boundary(range.end)
            })
            .collect();
        if self.text == text.as_str()
            && let [BlockNode::CodeBlock(code)] = self.parsed_content.document.blocks.as_slice()
        {
            if code.set_highlights(highlights) {
                cx.notify();
            }
            return;
        }

        self.reset_selection();
        // Retire any earlier asynchronous markup parse before installing an
        // externally styled document, so it cannot replace this literal text.
        self.revision += 1;
        self.text = text.to_string();
        self.parsed_error = None;
        let code = CodeBlock::new(
            text.clone(),
            None,
            &cx.theme().highlight_theme,
            Some(Span {
                start: 0,
                end: text.len(),
            }),
        );
        code.set_highlights(highlights);
        self.parsed_content = ParsedContent {
            document: ParsedDocument {
                source: text,
                blocks: vec![BlockNode::CodeBlock(code)],
            },
            ..Default::default()
        };
        cx.notify();
    }
}
