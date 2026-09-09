use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use gpui::{HighlightStyle, Hsla, SharedString};
use gpui_component::highlighter::{HighlightTheme, LanguageRegistry, SyntaxHighlighter};
use ropey::Rope;

use crate::transcript::code::ansi::{AnsiStyle, AnsiText};
use crate::transcript::code::source::{CodeSource, command_syntax};
use crate::transcript::virtual_code::transcript_segments;
use crate::transcript::{should_virtualize_transcript, strip_read_gutter};

const MAX_SYNTAX_BYTES: usize = 2 * 1024 * 1024;
const PARSE_BUDGET: Duration = Duration::from_millis(25);

struct SyntaxRegion {
    range: Range<usize>,
    language: String,
    highlighter: Option<SyntaxHighlighter>,
}

pub(super) struct PreparedCode {
    pub(super) text: SharedString,
    pub(super) segments: Arc<Vec<Range<usize>>>,
    pub(super) widest_segment: usize,
    pub(super) virtualized: bool,
    syntax: Vec<SyntaxRegion>,
    ansi: Vec<(Range<usize>, AnsiStyle)>,
}

impl PreparedCode {
    pub(super) fn new(source: &CodeSource) -> Self {
        let normalized = source
            .strip_gutter
            .then(|| strip_read_gutter(&source.output))
            .flatten();
        let output = AnsiText::parse(normalized.as_deref().unwrap_or(&source.output));
        let mut text = String::new();
        let mut syntax = Vec::new();
        if let Some(command) = &source.command {
            text.push_str("$ ");
            let (language, range) = command_syntax(command);
            syntax.push(SyntaxRegion {
                range: 2 + range.start..2 + range.end,
                language: language.into(),
                highlighter: None,
            });
            text.push_str(command);
            if !output.text.is_empty() {
                text.push_str("\n\n");
            }
        }
        let output_start = text.len();
        let language = source.output_language(&output.text);
        if !language.is_empty() && !output.text.is_empty() {
            syntax.push(SyntaxRegion {
                range: output_start..output_start + output.text.len(),
                language: language.into(),
                highlighter: None,
            });
        }
        text.push_str(&output.text);
        let ansi = output
            .spans
            .into_iter()
            .map(|(range, style)| (output_start + range.start..output_start + range.end, style))
            .collect();
        let segments = transcript_segments(&text);
        let widest_segment = segments
            .iter()
            .enumerate()
            .max_by_key(|(_, range)| range.len())
            .map_or(0, |(index, _)| index);
        let virtualized = should_virtualize_transcript(&text);
        Self {
            text: text.into(),
            segments: Arc::new(segments),
            widest_segment,
            virtualized,
            syntax,
            ansi,
        }
    }

    /// Parsing is performed by the background worker. A large or pathological
    /// source keeps its ANSI colors and readable text when syntax exceeds the
    /// budget; each command and output has an independent allowance.
    pub(super) fn parse_syntax(&mut self) {
        for region in &mut self.syntax {
            if region.range.len() > MAX_SYNTAX_BYTES
                || LanguageRegistry::singleton()
                    .language(&region.language)
                    .is_none()
            {
                continue;
            }
            let mut highlighter = SyntaxHighlighter::new(&region.language);
            let text = Rope::from_str(&self.text[region.range.clone()]);
            if highlighter.update(None, &text, Some(PARSE_BUDGET)) {
                region.highlighter = Some(highlighter);
            }
        }
    }

    /// Return row-local ranges. Queries touch only the visible source, and
    /// theme colors are resolved here so a theme change needs no new parse.
    pub(super) fn styles(
        &self,
        range: Range<usize>,
        theme: &HighlightTheme,
        foreground: Hsla,
        background: Hsla,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let mut syntax = Vec::new();
        for region in &self.syntax {
            let Some(highlighter) = &region.highlighter else {
                continue;
            };
            let start = range.start.max(region.range.start);
            let end = range.end.min(region.range.end);
            if start >= end {
                continue;
            }
            syntax.extend(
                highlighter
                    .styles(
                        &(start - region.range.start..end - region.range.start),
                        theme,
                    )
                    .into_iter()
                    .map(|(span, style)| {
                        (
                            span.start + region.range.start - range.start
                                ..span.end + region.range.start - range.start,
                            style,
                        )
                    }),
            );
        }
        let first = self
            .ansi
            .partition_point(|(span, _)| span.end <= range.start);
        let ansi: Vec<_> = self.ansi[first..]
            .iter()
            .take_while(|(span, _)| span.start < range.end)
            .map(|(span, style)| {
                (
                    span.start.max(range.start) - range.start
                        ..span.end.min(range.end) - range.start,
                    style.resolve(foreground, background),
                )
            })
            .collect();
        overlay_styles(&syntax, &ansi)
    }
}

/// Both inputs are ordered, non-overlapping runs. Resolve each interval with
/// ANSI applied last; an unordered active-style set cannot establish which
/// foreground color wins where syntax and terminal colors overlap.
fn overlay_styles(
    syntax: &[(Range<usize>, HighlightStyle)],
    ansi: &[(Range<usize>, HighlightStyle)],
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut boundaries = syntax
        .iter()
        .chain(ansi)
        .flat_map(|(range, _)| [range.start, range.end])
        .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut syntax_index = 0;
    let mut ansi_index = 0;
    let mut result: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    for pair in boundaries.windows(2) {
        let start = pair[0];
        while syntax
            .get(syntax_index)
            .is_some_and(|(range, _)| range.end <= start)
        {
            syntax_index += 1;
        }
        while ansi
            .get(ansi_index)
            .is_some_and(|(range, _)| range.end <= start)
        {
            ansi_index += 1;
        }
        let base = syntax
            .get(syntax_index)
            .filter(|(range, _)| range.contains(&start))
            .map(|(_, style)| *style);
        let overlay = ansi
            .get(ansi_index)
            .filter(|(range, _)| range.contains(&start))
            .map(|(_, style)| *style);
        let style = match (base, overlay) {
            (Some(base), Some(overlay)) => base.highlight(overlay),
            (Some(style), None) | (None, Some(style)) => style,
            (None, None) => continue,
        };
        if let Some((range, previous)) = result.last_mut()
            && *previous == style
            && range.end == start
        {
            range.end = pair[1];
        } else {
            result.push((start..pair[1], style));
        }
    }
    result
}
