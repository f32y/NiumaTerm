use std::ops::Range;
use std::sync::Arc;

use gpui::{Bounds, ElementId, HighlightStyle, Hsla, IntoElement, Pixels, SharedString};
use parking_lot::Mutex;

use crate::text::format::markdown;
use crate::text::inline::{Inline, InlineState};
use crate::text::node::{BlockNode, NodeContext, Paragraph, Span};

pub fn highlighted_text(
    id: impl Into<ElementId>,
    text: SharedString,
    styles: Vec<(Range<usize>, HighlightStyle)>,
    search: TextSearch,
    offset: usize,
) -> impl IntoElement {
    let mut state = InlineState::default();
    state.set_text(text);
    Inline::new(id, Arc::new(state.into()), Vec::new(), styles, None).search(
        Some(search),
        0,
        offset,
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextSearchMatch {
    pub source: Range<usize>,
    pub range: Range<usize>,
}

/// Search decoration is independent of the document's text selection.
#[derive(Clone)]
pub struct TextSearch {
    pub matches: Arc<Vec<TextSearchMatch>>,
    pub active: Option<usize>,
    pub background: Hsla,
    pub active_background: Hsla,
    pub active_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
}

impl TextSearch {
    pub(crate) fn inline_ranges(
        &self,
        anchor: usize,
        offset: usize,
        len: usize,
    ) -> Vec<(Range<usize>, bool)> {
        self.matches
            .iter()
            .enumerate()
            .filter_map(|(index, found)| {
                if found.source.start != anchor {
                    return None;
                }
                let start = found.range.start.max(offset);
                let end = found.range.end.min(offset + len);
                (start < end).then(|| (start - offset..end - offset, self.active == Some(index)))
            })
            .collect()
    }
}

pub fn markdown_matches(
    text: &str,
    query: &LiteralSearch,
) -> Result<Vec<TextSearchMatch>, SharedString> {
    let document = markdown::parse(text, &mut NodeContext::default())?;
    let mut matches = Vec::new();
    for block in document.blocks.iter() {
        search_block(block, query, &mut matches);
    }
    Ok(matches)
}

pub fn plain_matches(text: &str, query: &LiteralSearch) -> Vec<TextSearchMatch> {
    query
        .ranges(text)
        .into_iter()
        .map(|range| TextSearchMatch {
            source: 0..text.len(),
            range,
        })
        .collect()
}

fn search_paragraph(
    paragraph: &Paragraph,
    query: &LiteralSearch,
    matches: &mut Vec<TextSearchMatch>,
) {
    let text: String = paragraph
        .children
        .iter()
        .filter(|child| child.image.is_none())
        .map(|child| child.text.as_ref())
        .collect();
    append_matches(&text, paragraph.span, query, matches);
}

fn append_matches(
    text: &str,
    span: Option<Span>,
    query: &LiteralSearch,
    matches: &mut Vec<TextSearchMatch>,
) {
    let source = span.map_or(0..text.len(), |span| span.start..span.end);
    matches.extend(query.ranges(text).into_iter().map(|range| TextSearchMatch {
        source: source.clone(),
        range,
    }));
}

fn search_block(block: &BlockNode, query: &LiteralSearch, matches: &mut Vec<TextSearchMatch>) {
    match block {
        BlockNode::Root { children, .. }
        | BlockNode::Blockquote { children, .. }
        | BlockNode::List { children, .. }
        | BlockNode::ListItem { children, .. } => {
            for child in children {
                search_block(child, query, matches);
            }
        }
        BlockNode::Paragraph(paragraph)
        | BlockNode::Heading {
            children: paragraph,
            ..
        } => search_paragraph(paragraph, query, matches),
        BlockNode::CodeBlock(code) => append_matches(&code.code(), code.span, query, matches),
        BlockNode::Table(table) => {
            for row in &table.children {
                for cell in &row.children {
                    search_paragraph(&cell.children, query, matches);
                }
            }
        }
        _ => {}
    }
}

/// Literal Unicode lowercase matching with byte offsets in the original text.
#[derive(Clone, Debug)]
pub struct LiteralSearch {
    folded: String,
}

impl LiteralSearch {
    pub fn new(query: &str) -> Self {
        Self {
            folded: query.chars().flat_map(char::to_lowercase).collect(),
        }
    }

    pub fn ranges(&self, text: &str) -> Vec<Range<usize>> {
        if self.folded.is_empty() {
            return Vec::new();
        }
        let mut folded = String::with_capacity(text.len());
        let mut boundaries = Vec::with_capacity(text.chars().count() + 1);
        for (offset, ch) in text.char_indices() {
            boundaries.push((folded.len(), offset));
            folded.extend(ch.to_lowercase());
        }
        boundaries.push((folded.len(), text.len()));
        folded
            .match_indices(&self.folded)
            .filter_map(|(start, matched)| {
                let end = start + matched.len();
                let start = boundaries
                    .binary_search_by_key(&start, |&(at, _)| at)
                    .ok()?;
                let end = boundaries.binary_search_by_key(&end, |&(at, _)| at).ok()?;
                Some(boundaries[start].1..boundaries[end].1)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::rgba;
    use parking_lot::Mutex;

    use crate::text::search::{LiteralSearch, TextSearch, markdown_matches, plain_matches};

    #[test]
    fn highlight_ranges_clip_matches_to_visible_code_segments() {
        let text = "needle before\nvisible needle\nneedle after";
        let search = TextSearch {
            matches: Arc::new(plain_matches(text, &LiteralSearch::new("needle"))),
            active: Some(1),
            background: rgba(0).into(),
            active_background: rgba(0).into(),
            active_bounds: Arc::new(Mutex::new(None)),
        };
        assert_eq!(search.inline_ranges(0, 14, 14), vec![(8..14, true)]);
        assert_eq!(search.inline_ranges(0, 23, 3), vec![(0..3, true)]);
        assert!(search.inline_ranges(0, 6, 8).is_empty());
        assert!(search.inline_ranges(0, text.len(), 0).is_empty());
    }

    #[test]
    fn markdown_search_uses_display_text_and_distinct_table_cells() {
        let source = "Hello **world** [link](https://hidden.example)\n\n| one | two |\n| --- | --- |\n| hello | hello |\n\n```rs\nHELLO\n```";
        assert_eq!(
            markdown_matches(source, &LiteralSearch::new("hello"))
                .unwrap()
                .len(),
            4
        );
        assert!(
            markdown_matches(source, &LiteralSearch::new("hidden.example"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            markdown_matches(source, &LiteralSearch::new("Hello world link"))
                .unwrap()
                .len(),
            1
        );
        let hits = markdown_matches(source, &LiteralSearch::new("hello")).unwrap();
        assert_ne!(hits[1].source.start, hits[2].source.start);
    }

    #[test]
    fn unicode_ranges_refer_to_original_bytes() {
        let text = "A中文İb 中я";
        assert_eq!(LiteralSearch::new("中文").ranges(text), vec![1..7]);
        assert_eq!(LiteralSearch::new("i\u{307}B").ranges(text), vec![7..10]);
        assert!(LiteralSearch::new("i").ranges("İ").is_empty());
        assert_eq!(LiteralSearch::new("Я").ranges(text), vec![14..16]);
        assert_eq!(LiteralSearch::new("ΟΣ").ranges("ΟΣ"), vec![0..4]);
    }

    #[test]
    fn empty_literal_and_repeated_matches() {
        assert!(LiteralSearch::new("").ranges("hello").is_empty());
        assert_eq!(LiteralSearch::new(".*").ranges("a.*b.*"), vec![1..3, 4..6]);
        assert_eq!(LiteralSearch::new("aa").ranges("aaaaa"), vec![0..2, 2..4]);
    }
}
