use std::sync::Arc;

use gpui::{
    Context, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext, Window, div, px,
};

use crate::highlighter::HighlightTheme;
use crate::text::{CODE_BLOCK_HIGHLIGHT_CALLS, TextView, TextViewStyle};

struct StyledCode {
    theme: Arc<HighlightTheme>,
}

impl Render for StyledCode {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(600.)).child(
            TextView::markdown("code", "```json\n{\"value\": 42}\n```").style(TextViewStyle {
                highlight_theme: self.theme.clone(),
                ..TextViewStyle::default()
            }),
        )
    }
}

#[gpui::test]
fn styled_code_reuses_highlights_across_layouts(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let (view, cx) = cx.add_window_view(|_, _| StyledCode {
        theme: HighlightTheme::default_dark(),
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let initial = CODE_BLOCK_HIGHLIGHT_CALLS.with(|calls| calls.get());
    assert!(initial > 0);

    for _ in 0..5 {
        view.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
    assert_eq!(
        CODE_BLOCK_HIGHLIGHT_CALLS.with(|calls| calls.get()),
        initial
    );

    view.update(cx, |view, cx| {
        view.theme = Arc::new((*HighlightTheme::default_light()).clone());
        cx.notify();
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert_eq!(
        CODE_BLOCK_HIGHLIGHT_CALLS.with(|calls| calls.get()),
        initial + 1
    );
}
