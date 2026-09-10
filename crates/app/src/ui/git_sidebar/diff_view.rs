use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, ListHorizontalSizingBehavior, UniformListScrollHandle, div, px, uniform_list,
};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, h_flex};
use nmt_app_terminal::metrics;
use nmt_i18n::i18n;
use unicode_width::UnicodeWidthStr;

use crate::ui::composition::GitColors;
use crate::ui::git_sidebar::scrollbar;
use crate::ui::git_status::{DiffLine, DiffLineKind};

#[derive(Default)]
pub(super) struct DiffView {
    lines: Rc<[DiffLine]>,
    widest: usize,
    gutter_width: f32,
}

impl DiffView {
    pub(super) fn new(mut lines: Vec<DiffLine>) -> Self {
        for line in &mut lines {
            if line.text.contains('\t') {
                line.text = line.text.replace('\t', "    ").into();
            }
        }
        let widest = lines
            .iter()
            .enumerate()
            .max_by_key(|(_, line)| UnicodeWidthStr::width(line.text.as_ref()))
            .map_or(0, |(index, _)| index);
        let max_number = lines
            .iter()
            .flat_map(|line| [line.old_line, line.new_line])
            .flatten()
            .max()
            .unwrap_or(1);
        let digits = (max_number.ilog10() + 1).max(3);
        Self {
            lines: lines.into(),
            widest,
            gutter_width: digits as f32 * 8.0 + 12.0,
        }
    }

    pub(super) fn render(&self, scroll: &UniformListScrollHandle, cx: &App) -> AnyElement {
        if self.lines.is_empty() {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(i18n("sidebar-git-no-text-diff"))
                .into_any_element();
        }
        let lines = self.lines.clone();
        let gutter_width = self.gutter_width;
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .font_family(metrics::font_family(cx))
            .text_size(px(12.0))
            .child(
                uniform_list("git-diff", lines.len(), move |range, _, cx| {
                    let theme = cx.theme();
                    let colors = GitColors::new(cx);
                    range
                        .map(|index| {
                            let line = &lines[index];
                            let mut row = h_flex()
                                .w_full()
                                .h(px(20.0))
                                .flex_none()
                                .line_height(px(20.0))
                                .whitespace_nowrap()
                                .text_color(theme.foreground);
                            row = match line.kind {
                                DiffLineKind::Added => row.bg(colors.added_background),
                                DiffLineKind::Removed => row.bg(colors.removed_background),
                                DiffLineKind::Hunk
                                | DiffLineKind::Notice
                                | DiffLineKind::Truncated => row.text_color(theme.muted_foreground),
                                DiffLineKind::Context => row,
                            };
                            let number = |value: Option<u64>| {
                                div()
                                    .w(px(gutter_width))
                                    .flex_none()
                                    .pr(px(6.0))
                                    .text_right()
                                    .text_color(theme.muted_foreground)
                                    .child(value.map(|n| n.to_string()).unwrap_or_default())
                            };
                            let row = row
                                .child(
                                    h_flex()
                                        .flex_none()
                                        .border_r_1()
                                        .border_color(theme.sidebar_border)
                                        .child(number(line.old_line))
                                        .child(number(line.new_line)),
                                )
                                .child(div().flex_none().px_2().child(line.text.clone()));
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
            .child(scrollbar(scroll))
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
