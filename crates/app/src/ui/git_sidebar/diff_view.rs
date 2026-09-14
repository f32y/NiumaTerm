use std::iter;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use app::terminal_tab::metrics;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Entity, HighlightStyle, Hsla, ListHorizontalSizingBehavior, Role,
    ScrollStrategy, StyledText, UniformListScrollHandle, div, px, uniform_list,
};
use gpui_component::highlighter::{LanguageRegistry, SyntaxHighlighter};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, h_flex};
use ropey::Rope;
use rust_i18n::t;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::ui::composition::GitColors;
use crate::ui::git_sidebar::GitSidebar;
use crate::ui::git_status::{DiffLine, DiffLineKind};

pub(super) const ROW_HEIGHT: f32 = 24.0;

pub(super) struct PreparedDiff {
    lines: Vec<DiffLine>,
    words: Vec<Option<Range<usize>>>,
    old_syntax: Option<SyntaxHighlighter>,
    new_syntax: Option<SyntaxHighlighter>,
    offsets: Vec<usize>,
    changes: Vec<usize>,
}

#[derive(Clone)]
struct DisplayRow {
    source: usize,
    range: Range<usize>,
    first: bool,
}

#[derive(Default)]
pub(super) struct DiffView {
    prepared: Option<Arc<PreparedDiff>>,
    rows: Arc<Vec<DisplayRow>>,
    widest: usize,
    gutter_width: f32,
    wrap_columns: Option<usize>,
    active_change: Option<usize>,
}

impl DiffView {
    #[cfg(test)]
    pub(super) fn new(lines: Vec<DiffLine>) -> Self {
        Self::from_prepared(Self::prepare(lines, ""))
    }

    pub(super) fn prepare(mut lines: Vec<DiffLine>, path: &str) -> PreparedDiff {
        let mut old = String::new();
        let mut new = String::new();
        let mut offsets = Vec::with_capacity(lines.len());
        let mut changes = Vec::new();
        let mut was_change = false;

        for (index, line) in lines.iter_mut().enumerate() {
            if line.text.contains('\t') {
                line.text = line.text.replace('\t', "    ").into();
            }

            let changed = matches!(line.kind, DiffLineKind::Added | DiffLineKind::Removed);

            if changed && !was_change {
                changes.push(index);
            }

            was_change = changed;

            offsets.push(if line.kind == DiffLineKind::Removed {
                old.len()
            } else {
                new.len()
            });

            if line.old_line.is_some() {
                old.push_str(&line.text);
                old.push('\n');
            }

            if line.new_line.is_some() {
                new.push_str(&line.text);
                new.push('\n');
            }
        }

        let mut words = vec![None; lines.len()];
        let mut index = 0;

        while index < lines.len() {
            if lines[index].kind != DiffLineKind::Removed {
                index += 1;

                continue;
            }

            let old_start = index;

            while index < lines.len() && lines[index].kind == DiffLineKind::Removed {
                index += 1;
            }

            let new_start = index;

            while index < lines.len() && lines[index].kind == DiffLineKind::Added {
                index += 1;
            }

            for offset in 0..(new_start - old_start).min(index - new_start) {
                let a = old_start + offset;
                let b = new_start + offset;
                let (left, right) = changed_ranges(&lines[a].text, &lines[b].text);

                words[a] = left;
                words[b] = right;
            }
        }

        let language = match Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
        {
            "rs" => "rust",
            "ts" | "tsx" => "typescript",
            "js" | "jsx" | "mjs" | "cjs" => "javascript",
            "py" => "python",
            "json" => "json",
            "toml" => "toml",
            "html" | "vue" => "html",
            "css" | "scss" => "css",
            "md" => "markdown",
            "sh" | "zsh" => "bash",
            "c" | "h" => "c",
            "cpp" | "hpp" => "cpp",
            "yml" | "yaml" => "yaml",
            _ => "",
        };

        let parse = |text: &str| {
            if text.len() > 2 * 1024 * 1024
                || LanguageRegistry::singleton().language(language).is_none()
            {
                return None;
            }

            let mut highlighter = SyntaxHighlighter::new(language);

            highlighter
                .update(None, &Rope::from_str(text), Some(Duration::from_millis(25)))
                .then_some(highlighter)
        };

        PreparedDiff {
            lines,
            words,
            old_syntax: parse(&old),
            new_syntax: parse(&new),
            offsets,
            changes,
        }
    }

    pub(super) fn update(&mut self, prepared: PreparedDiff) {
        if self
            .prepared
            .as_ref()
            .is_some_and(|current| current.lines == prepared.lines)
        {
            return;
        }

        let columns = self.wrap_columns;

        *self = Self::from_prepared(prepared);

        self.set_wrap(columns);
    }

    pub(super) fn from_prepared(prepared: PreparedDiff) -> Self {
        let max_number = prepared
            .lines
            .iter()
            .flat_map(|line| [line.old_line, line.new_line])
            .flatten()
            .max()
            .unwrap_or(1);

        let mut view = Self {
            gutter_width: (max_number.ilog10() + 1).max(3) as f32 * 8.0 + 10.0,
            prepared: Some(Arc::new(prepared)),
            ..Self::default()
        };

        view.rebuild_rows();

        view
    }

    pub(super) fn len(&self) -> usize {
        self.prepared.as_ref().map_or(0, |data| data.lines.len())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(super) fn line(&self, index: usize) -> Option<&DiffLine> {
        self.prepared.as_ref()?.lines.get(index)
    }

    pub(super) fn change_count(&self) -> usize {
        self.prepared.as_ref().map_or(0, |data| data.changes.len())
    }

    pub(super) fn current_change(&self, scroll: &UniformListScrollHandle) -> usize {
        if let Some(index) = self.active_change {
            return index;
        }

        let row =
            ((-scroll.0.borrow().base_handle.offset().y.as_f32()) / ROW_HEIGHT).max(0.0) as usize;

        let source = self.rows.get(row).map_or(0, |row| row.source);

        self.prepared.as_ref().map_or(0, |data| {
            data.changes
                .partition_point(|&index| index <= source)
                .saturating_sub(1)
        })
    }

    pub(super) fn jump_change(&mut self, index: usize, scroll: &UniformListScrollHandle) {
        if let Some(source) = self
            .prepared
            .as_ref()
            .and_then(|data| data.changes.get(index))
        {
            let row = self.rows.partition_point(|row| row.source < *source);

            self.active_change = Some(index);

            scroll.scroll_to_item(row, ScrollStrategy::Top);
        }
    }

    pub(super) fn change_positions(&self) -> Vec<(usize, f32, bool)> {
        self.prepared.as_ref().map_or_else(Vec::new, |data| {
            data.changes
                .iter()
                .enumerate()
                .map(|(index, &source)| {
                    let row = self.rows.partition_point(|row| row.source < source);

                    (
                        index,
                        row as f32 / self.rows.len().max(1) as f32,
                        data.lines[source].kind == DiffLineKind::Removed,
                    )
                })
                .collect()
        })
    }

    pub(super) fn set_wrap(&mut self, columns: Option<usize>) {
        if self.wrap_columns != columns {
            self.wrap_columns = columns;

            self.rebuild_rows();
        }
    }

    fn rebuild_rows(&mut self) {
        let Some(data) = &self.prepared else { return };

        let mut rows = Vec::new();

        for (source, line) in data.lines.iter().enumerate() {
            let ranges = self.wrap_columns.map_or_else(
                || iter::once(0..line.text.len()).collect(),
                |columns| wrapped_ranges(&line.text, columns),
            );

            for (index, range) in ranges.into_iter().enumerate() {
                rows.push(DisplayRow {
                    source,
                    range,
                    first: index == 0,
                });
            }
        }

        self.widest = rows
            .iter()
            .enumerate()
            .max_by_key(|(_, row)| {
                UnicodeWidthStr::width(&data.lines[row.source].text[row.range.clone()])
            })
            .map_or(0, |(index, _)| index);

        self.rows = Arc::new(rows);
    }

    pub(super) fn render(
        &self,
        scroll: &UniformListScrollHandle,
        owner: Option<Entity<GitSidebar>>,
        selected: Option<usize>,
        cx: &App,
    ) -> AnyElement {
        let Some(data) = self.prepared.clone().filter(|data| !data.lines.is_empty()) else {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(t!("sidebar-git-no-text-diff"))
                .into_any_element();
        };

        let rows = self.rows.clone();
        let gutter_width = self.gutter_width;
        let base = scroll.0.borrow().base_handle.clone();
        let scroll_owner = owner.clone();

        div()
            .h_full()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .relative()
            .overflow_hidden()
            .font_family(metrics::font_family(cx))
            .text_size(px(13.0))
            .on_scroll_wheel(move |_, _, cx| {
                if let Some(owner) = &scroll_owner {
                    owner.update(cx, |this, cx| {
                        this.diff.active_change = None;

                        cx.notify();
                    });
                }
            })
            .child(
                uniform_list("git-diff", rows.len(), move |range, _, cx| {
                    let theme = cx.theme();
                    let colors = GitColors::new(cx);

                    range
                        .map(|index| {
                            let displayed = &rows[index];
                            let source = displayed.source;
                            let line = &data.lines[source];

                            let background = match line.kind {
                                DiffLineKind::Added => colors.added_background,
                                DiffLineKind::Removed => colors.removed_background,
                                DiffLineKind::Hunk => theme.muted,
                                _ => theme.sidebar,
                            };

                            let mut styles = row_styles(&data, source, displayed.range.clone(), cx);

                            if let Some(word) = &data.words[source] {
                                let color = if line.kind == DiffLineKind::Removed {
                                    colors.removed_word
                                } else {
                                    colors.added_word
                                };

                                styles = overlay_word(styles, word, &displayed.range, color);
                            }

                            let number = |value: Option<u64>| {
                                div()
                                    .w(px(gutter_width))
                                    .flex_none()
                                    .pr(px(7.0))
                                    .text_right()
                                    .text_size(px(11.0))
                                    .child(if displayed.first {
                                        value.map(|n| n.to_string()).unwrap_or_default()
                                    } else {
                                        String::new()
                                    })
                            };

                            let selectable = line.old_line.is_some() || line.new_line.is_some();

                            let gutter = h_flex()
                                .id(("diff-line", index))
                                .when(selectable, |this| this.role(Role::Button))
                                .absolute()
                                .left(-base.offset().x)
                                .top_0()
                                .h(px(ROW_HEIGHT))
                                .bg(background)
                                .text_color(theme.muted_foreground)
                                .when(selectable, |this| this.cursor_pointer())
                                .aria_label(format!(
                                    "{} {}",
                                    t!("git-quote-line"),
                                    line.new_line.or(line.old_line).unwrap_or(0)
                                ))
                                .child(number(line.old_line))
                                .child(number(line.new_line))
                                .child(div().w(px(18.0)).text_center().child(if !displayed.first {
                                    ""
                                } else {
                                    match line.kind {
                                        DiffLineKind::Added => "+",
                                        DiffLineKind::Removed => "−",
                                        _ => "",
                                    }
                                }))
                                .on_click({
                                    let owner = owner.clone();

                                    move |_, window, cx| {
                                        if selectable && let Some(owner) = &owner {
                                            owner.update(cx, |view, cx| {
                                                view.selected_line = Some(source);

                                                view.focus(window, cx);

                                                cx.notify();
                                            });
                                        }
                                    }
                                });

                            let row = h_flex()
                                .w_full()
                                .h(px(ROW_HEIGHT))
                                .flex_none()
                                .relative()
                                .line_height(px(ROW_HEIGHT))
                                .whitespace_nowrap()
                                .bg(background)
                                .text_color(theme.foreground)
                                .when(selected == Some(source), |row| {
                                    row.border_y_1().border_color(theme.primary.opacity(0.5))
                                })
                                .child(
                                    div()
                                        .flex_none()
                                        .pl(px(gutter_width * 2.0 + 25.0))
                                        .pr_4()
                                        .child(
                                            StyledText::new(
                                                line.text[displayed.range.clone()].to_string(),
                                            )
                                            .with_highlights(styles),
                                        ),
                                )
                                .child(gutter);

                            #[cfg(test)]
                            let row = row.debug_selector(move || format!("diff-row-{index}"));

                            row
                        })
                        .collect::<Vec<_>>()
                })
                .with_width_from_item(Some(self.widest))
                .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                .track_scroll(scroll)
                .size_full()
                .pb(px(12.0)),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(12.0))
                    .child(Scrollbar::vertical(scroll)),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(12.0))
                    .child(Scrollbar::horizontal(scroll)),
            )
            .into_any_element()
    }
}

fn row_styles(
    data: &PreparedDiff,
    source: usize,
    range: Range<usize>,
    cx: &App,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let line = &data.lines[source];

    if line.old_line.is_none() && line.new_line.is_none() {
        return Vec::new();
    }

    let syntax = if line.kind == DiffLineKind::Removed {
        &data.old_syntax
    } else {
        &data.new_syntax
    };

    let start = data.offsets[source] + range.start;
    let end = data.offsets[source] + range.end;

    syntax.as_ref().map_or_else(Vec::new, |syntax| {
        syntax
            .styles(&(start..end), cx.theme().highlight_theme.as_ref())
            .into_iter()
            .filter_map(|(span, style)| {
                let left = span.start.max(start);
                let right = span.end.min(end);

                (left < right).then_some((left - start..right - start, style))
            })
            .collect()
    })
}

fn overlay_word(
    styles: Vec<(Range<usize>, HighlightStyle)>,
    word: &Range<usize>,
    displayed: &Range<usize>,
    color: Hsla,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let start = word.start.max(displayed.start);
    let end = word.end.min(displayed.end);

    if start >= end {
        return styles;
    }

    let word = start - displayed.start..end - displayed.start;

    let mut boundaries: Vec<_> = styles
        .iter()
        .flat_map(|(span, _)| [span.start, span.end])
        .chain([word.start, word.end])
        .collect();

    boundaries.sort_unstable();
    boundaries.dedup();

    let mut index = 0;

    boundaries
        .windows(2)
        .map(|bounds| {
            while styles
                .get(index)
                .is_some_and(|(span, _)| span.end <= bounds[0])
            {
                index += 1;
            }

            let mut style = styles
                .get(index)
                .filter(|(span, _)| span.contains(&bounds[0]))
                .map(|(_, style)| *style)
                .unwrap_or_default();

            if word.contains(&bounds[0]) {
                style.background_color = Some(color);
            }

            (bounds[0]..bounds[1], style)
        })
        .collect()
}

fn changed_ranges(a: &str, b: &str) -> (Option<Range<usize>>, Option<Range<usize>>) {
    let prefix = a
        .chars()
        .zip(b.chars())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();

    let suffix = a[prefix..]
        .chars()
        .rev()
        .zip(b[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();

    let left = prefix..a.len() - suffix;
    let right = prefix..b.len() - suffix;

    (
        (!left.is_empty()).then_some(left),
        (!right.is_empty()).then_some(right),
    )
}

fn wrapped_ranges(text: &str, columns: usize) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut width = 0;

    for (offset, character) in text.char_indices() {
        let advance = character.width().unwrap_or(0);

        if width + advance > columns.max(1) && offset > start {
            ranges.push(start..offset);

            start = offset;
            width = 0;
        }

        width += advance;
    }

    ranges.push(start..text.len());

    ranges
}
