use gpui::prelude::*;
use gpui::{
    Context, ListHorizontalSizingBehavior, Render, StyledText, Window, div, px, uniform_list,
};
use gpui_component::ActiveTheme as _;
use gpui_component::scroll::Scrollbar;
use gpui_component::spinner::Spinner;
use gpui_component::text::TextView;

use crate::settings::{AgentSettings, UI_RADIUS};
use crate::transcript::code::CodeView;
use crate::transcript::render::TRANSCRIPT_LINE_HEIGHT;
use crate::transcript::render::text_style::work_detail_text_style;

impl Render for CodeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(prepared) = self.prepared.clone() else {
            return div()
                .w_full()
                .h(px(256.))
                .p_3()
                .child(Spinner::new())
                .into_any_element();
        };
        let theme = cx.theme().highlight_theme.clone();
        let foreground = cx.theme().foreground;
        let background = *cx.theme().tokens.muted;

        if prepared.virtualized {
            let settings = cx.global::<AgentSettings>();
            let font_size = px(settings.transcript_font_size);
            let line_height = font_size * TRANSCRIPT_LINE_HEIGHT;
            let source = prepared.clone();
            return div()
                .id("code-output")
                .w_full()
                .h(px(256.))
                .relative()
                .overflow_hidden()
                .rounded(UI_RADIUS)
                .bg(background)
                .text_color(foreground)
                .font(settings.transcript_font())
                .text_size(font_size)
                .occlude()
                .child(
                    uniform_list("code-lines", prepared.segments.len(), move |rows, _, _| {
                        rows.filter_map(|row| {
                            let range = source.segments.get(row)?.clone();
                            let styles =
                                source.styles(range.clone(), &theme, foreground, background);
                            Some(
                                div()
                                    .h(line_height)
                                    .flex_none()
                                    .line_height(line_height)
                                    .whitespace_nowrap()
                                    .child(
                                        StyledText::new(source.text[range].to_string())
                                            .with_highlights(styles),
                                    ),
                            )
                        })
                        .collect::<Vec<_>>()
                    })
                    .with_width_from_item(Some(prepared.widest_segment))
                    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                    .track_scroll(&self.virtual_scroll)
                    .size_full()
                    .p_3(),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(px(16.))
                        .child(Scrollbar::vertical(&self.virtual_scroll)),
                )
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(px(16.))
                        .child(Scrollbar::horizontal(&self.virtual_scroll)),
                )
                .into_any_element();
        }

        let styles = prepared.styles(0..prepared.text.len(), &theme, foreground, background);
        self.text_view.update(cx, |view, cx| {
            view.set_highlighted_code(prepared.text.clone(), styles, cx)
        });
        div()
            .w_full()
            .relative()
            .child(
                div()
                    .id("code-output")
                    .w_full()
                    .max_h(px(256.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .occlude()
                    .text_color(foreground)
                    .child(
                        TextView::new(&self.text_view)
                            .style(work_detail_text_style(cx))
                            .selectable(true),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(16.))
                    .child(Scrollbar::vertical(&self.scroll)),
            )
            .into_any_element()
    }
}
