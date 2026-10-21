use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use gpui::{HighlightStyle, Hsla, SharedString};
use gpui_component::highlighter::{HighlightTheme, LanguageRegistry, SyntaxHighlighter};
use gpui_component::input::RopeExt as _;
use ropey::Rope;
use tree_sitter::{InputEdit, Point};

use crate::agent_tab::transcript::code::ansi::AnsiStyle;
use crate::agent_tab::transcript::code::rows::CodeRows;
use crate::agent_tab::transcript::code::source::command_syntax;
#[cfg(test)]
use crate::agent_tab::transcript::code::streaming_tests::{record_styles, record_syntax};

const MAX_SYNTAX_BYTES: usize = 2 * 1024 * 1024;
const PARSE_BUDGET: Duration = Duration::from_millis(25);

#[derive(Clone)]
struct SyntaxRegion {
    range: Range<usize>,
    language: String,
    highlighter: Option<Arc<SyntaxHighlighter>>,
}

#[derive(Clone)]
pub(super) struct PreparedCode {
    pub(super) text: Rope,
    pub(super) short_text: SharedString,
    pub(super) segments: CodeRows,
    pub(super) widest_segment: usize,
    pub(super) virtualized: bool,
    syntax: Vec<SyntaxRegion>,
    output_start: Option<usize>,
    command: bool,
}

impl PreparedCode {
    pub(super) fn new(command: Option<&str>) -> Self {
        let mut text = String::new();
        let mut syntax = Vec::new();

        if let Some(command) = command {
            text.push_str("$ ");

            let (language, range) = command_syntax(command);

            syntax.push(SyntaxRegion {
                range: 2 + range.start..2 + range.end,
                language: language.into(),
                highlighter: None,
            });

            text.push_str(command);
        }

        let mut result = Self {
            text: Rope::from_str(&text),
            short_text: text.into(),
            segments: CodeRows::default(),
            widest_segment: 0,
            virtualized: false,
            syntax,
            output_start: None,
            command: command.is_some(),
        };

        result.segments.append(&result.text, Vec::new());
        result.refresh_layout();

        result
    }

    pub(super) fn append_output(
        &mut self,
        text: &str,
        spans: Vec<(Range<usize>, AnsiStyle)>,
        language: &str,
    ) {
        if text.is_empty() {
            return;
        }

        if self.output_start.is_none() {
            if self.command {
                self.text.insert(self.text.len(), "\n\n");
            }

            self.output_start = Some(self.text.len());
        }

        let start = self.text.len();

        self.text.insert(start, text);

        self.segments.append(
            &self.text,
            spans
                .into_iter()
                .map(|(range, style)| (start + range.start..start + range.end, style))
                .collect(),
        );

        let output_start = self.output_start.expect("output was appended");
        let index = usize::from(self.command);

        if language.is_empty() {
            self.syntax.truncate(index);
        } else if let Some(region) = self.syntax.get_mut(index) {
            region.range.end = self.text.len();

            if region.language != language {
                region.language = language.into();
                region.highlighter = None;
            }
        } else {
            self.syntax.push(SyntaxRegion {
                range: output_start..self.text.len(),
                language: language.into(),
                highlighter: None,
            });
        }

        self.refresh_layout();
    }

    fn refresh_layout(&mut self) {
        self.widest_segment = self.segments.widest();

        self.virtualized = self.text.len() >= VIRTUAL_TRANSCRIPT_MIN_BYTES
            || self.segments.len() > VIRTUAL_TRANSCRIPT_MIN_ROWS;

        self.short_text = if self.virtualized {
            SharedString::default()
        } else {
            self.text.to_string().into()
        };
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
                region.highlighter = None;

                continue;
            }

            let highlighter = region
                .highlighter
                .get_or_insert_with(|| Arc::new(SyntaxHighlighter::new(&region.language)));

            let old_end = highlighter.text().len();

            if old_end == region.range.len() {
                continue;
            }

            let old_position = highlighter.text().offset_to_point(old_end);
            let text = Rope::from(self.text.slice(region.range.clone()));
            let new_position = text.offset_to_point(text.len());

            let edit = InputEdit {
                start_byte: old_end,
                old_end_byte: old_end,
                new_end_byte: text.len(),
                start_position: Point::new(old_position.row, old_position.column),
                old_end_position: Point::new(old_position.row, old_position.column),
                new_end_position: Point::new(new_position.row, new_position.column),
            };

            #[cfg(test)]
            record_syntax(text.len() - old_end);

            if !Arc::make_mut(highlighter).update(Some(edit), &text, Some(PARSE_BUDGET)) {
                region.highlighter = None;
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
        #[cfg(test)]
        record_styles();

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

        let ansi: Vec<_> = self
            .segments
            .ansi(&range)
            .into_iter()
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

const VIRTUAL_TRANSCRIPT_MIN_BYTES: usize = 16 * 1024;
const VIRTUAL_TRANSCRIPT_MIN_ROWS: usize = 128;
pub(crate) const VIRTUAL_TRANSCRIPT_MAX_SEGMENT_BYTES: usize = 4 * 1024;

#[cfg(test)]
pub(crate) fn should_virtualize_transcript(text: &str) -> bool {
    text.len() >= VIRTUAL_TRANSCRIPT_MIN_BYTES
        || text.lines().take(VIRTUAL_TRANSCRIPT_MIN_ROWS + 1).count() > VIRTUAL_TRANSCRIPT_MIN_ROWS
}

pub(crate) fn transcript_segments(text: &str) -> Vec<Range<usize>> {
    let mut segments = Vec::new();
    let mut line_start = 0;

    for line in text.split_inclusive('\n') {
        let mut line_end = line_start + line.len();

        if line.ends_with('\n') {
            line_end -= 1;

            if line_end > line_start && text.as_bytes()[line_end - 1] == b'\r' {
                line_end -= 1;
            }
        }

        if line_start == line_end {
            segments.push(line_start..line_start);
        } else {
            let mut segment_start = line_start;

            while segment_start < line_end {
                let mut segment_end =
                    (segment_start + VIRTUAL_TRANSCRIPT_MAX_SEGMENT_BYTES).min(line_end);

                while !text.is_char_boundary(segment_end) {
                    segment_end -= 1;
                }

                segments.push(segment_start..segment_end);

                segment_start = segment_end;
            }
        }

        line_start += line.len();
    }

    if segments.is_empty() && !text.is_empty() {
        segments.push(0..text.len());
    }

    segments
}
