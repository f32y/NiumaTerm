use crate::text::state::*;
use gpui::TestAppContext;

#[gpui::test]
fn external_code_styles_keep_literal_text_and_selection_when_colors_change(
    cx: &mut TestAppContext,
) {
    use gpui::{HighlightStyle, rgb};
    cx.update(crate::init);
    let state = cx.update(|cx| cx.new(|cx| TextViewState::plain("", cx)));
    let text: gpui::SharedString = "$ echo\n\n**literal**\n```".into();
    state.update(cx, |state, cx| {
        state.set_highlighted_code(
            text.clone(),
            vec![(
                2..6,
                HighlightStyle {
                    color: Some(rgb(0xff0000).into()),
                    ..Default::default()
                },
            )],
            cx,
        );
        state.select_all = true;
        state.set_highlighted_code(
            text.clone(),
            vec![(
                2..6,
                HighlightStyle {
                    color: Some(rgb(0x00ff00).into()),
                    ..Default::default()
                },
            )],
            cx,
        );
        assert!(state.select_all, "a color update must preserve select-all");
        assert_eq!(state.source(), text);
        assert_eq!(state.selected_text(), format!("{text}\n"));
        let [node::BlockNode::CodeBlock(code)] = state.parsed_content.document.blocks.as_slice()
        else {
            panic!("expected literal code")
        };
        assert_eq!(
            code.external_highlights().unwrap()[0].1.color,
            Some(rgb(0x00ff00).into())
        );
    });
}

#[gpui::test]
fn plain_format_keeps_markup_verbatim(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let text = "*raw* `not code`\n# not a heading";
    let state = cx.update(|cx| cx.new(|cx| TextViewState::plain(text, cx)));
    cx.run_until_parked();

    state.read_with(cx, |state, _| {
        assert_eq!(state.source().as_str(), text);
        assert_eq!(state.parsed_content.document.blocks.len(), 1);
        // Block text carries the paragraph convention of a trailing
        // newline; the body must stay verbatim.
        assert_eq!(state.parsed_content.document.text(), format!("{text}\n"));
    });

    state.update(cx, |state, cx| {
        state.set_text("replaced", cx);
    });
    cx.run_until_parked();

    state.read_with(cx, |state, _| {
        assert_eq!(state.parsed_content.document.text(), "replaced\n");
    });
}
