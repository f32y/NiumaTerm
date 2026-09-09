use crate::text::MarkdownNode;
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
        assert_eq!(code.styles()[0].1.color, Some(rgb(0x00ff00).into()));
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

#[gpui::test]
fn set_text_then_push_str_appends_to_replaced_content(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown("old", cx)));
    cx.run_until_parked();

    state.update(cx, |state, cx| {
        state.set_text("", cx);
        state.push_str("new", cx);
        state.push_str(" text", cx);
    });
    cx.run_until_parked();

    state.read_with(cx, |state, _| {
        assert_eq!(state.text.as_str(), "new text");
        assert_eq!(state.source().as_str(), "new text");
    });

    state.update(cx, |state, cx| {
        state.set_text("", cx);
    });
    cx.run_until_parked();

    state.read_with(cx, |state, _| {
        assert_eq!(state.text.as_str(), "");
        assert_eq!(state.source().as_str(), "");
    });
}

#[test]
fn update_options_merge_keeps_latest_full_text() {
    let theme = HighlightTheme::default_light();
    let mut options = UpdateOptions {
        revision: 1,
        pending_text: "old".to_string(),
        append: true,
        highlight_theme: theme.clone(),
        markdown_extensions: Arc::default(),
    };

    options.merge(UpdateOptions {
        revision: 2,
        pending_text: "new".to_string(),
        append: false,
        highlight_theme: theme.clone(),
        markdown_extensions: Arc::default(),
    });
    options.merge(UpdateOptions {
        revision: 3,
        pending_text: " text".to_string(),
        append: true,
        highlight_theme: theme,
        markdown_extensions: Arc::default(),
    });

    assert_eq!(options.revision, 3);
    assert_eq!(options.pending_text, "new text");
    assert!(!options.append);
}

#[test]
fn update_future_yields_before_coalescing_all_queued_updates() {
    let theme = HighlightTheme::default_light();
    let (tx, rx) = unbounded::<UpdateOptions>();
    let (tx_result, rx_result) = unbounded::<ParsedUpdate>();
    let total_updates = 128;

    for revision in 1..=total_updates {
        tx.try_send(UpdateOptions {
            revision,
            pending_text: format!("{revision}\n"),
            append: revision != 1,
            highlight_theme: theme.clone(),
            markdown_extensions: Arc::default(),
        })
        .unwrap();
    }

    let mut future = Box::pin(UpdateFuture::new(TextViewFormat::Markdown, rx, tx_result));
    let waker = futures::task::noop_waker();
    let mut task_cx = std::task::Context::from_waker(&waker);

    assert!(matches!(
        std::future::Future::poll(future.as_mut(), &mut task_cx),
        Poll::Pending
    ));
    let parsed_update = rx_result.try_recv().expect("parse result");

    assert!(
        parsed_update.revision < total_updates,
        "single poll coalesced every queued update through revision {}",
        parsed_update.revision
    );

    assert!(matches!(
        std::future::Future::poll(future.as_mut(), &mut task_cx),
        Poll::Pending
    ));
    let parsed_update = rx_result.try_recv().expect("next parse result");
    assert_eq!(parsed_update.revision, total_updates);
}

#[gpui::test]
fn select_all_returns_rendered_text(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown("**quick** value", cx)));
    cx.run_until_parked();

    state.update(cx, |state, cx| {
        state.select_all(cx);
    });

    state.read_with(cx, |state, _| {
        assert!(state.has_view_selection());
        assert_eq!(state.selected_text().trim(), "quick value");
    });

    state.update(cx, |state, cx| {
        state.clear_selection(cx);
    });

    state.read_with(cx, |state, _| {
        assert!(!state.has_view_selection());
        assert_eq!(state.selected_text(), "");
    });
}

#[gpui::test]
fn set_markdown_extensions_reparses_existing_text(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown("$TSLA.US", cx)));
    cx.run_until_parked();

    let extensions = MarkdownExtensions::default().block_parser(|node, cx| {
        let markdown::mdast::Node::Paragraph(paragraph) = node else {
            return None;
        };
        let [markdown::mdast::Node::Text(text)] = paragraph.children.as_slice() else {
            return None;
        };
        let symbol = text.value.strip_prefix('$')?.to_string();
        let node_text = format!("${symbol}");

        Some(
            MarkdownNode::new("ticker", symbol)
                .text(node_text)
                .markdown(cx.node_source(node).unwrap_or_default()),
        )
    });

    state.update(cx, |state, cx| {
        state.set_markdown_extensions(Arc::new(extensions), cx);
    });
    cx.run_until_parked();

    state.read_with(cx, |state, _| {
        let node::BlockNode::Custom(node) = &state.parsed_content.document.blocks[0] else {
            panic!("expected custom markdown node");
        };
        assert_eq!(node.name(), "ticker");
        assert_eq!(node.data::<String>().map(String::as_str), Some("TSLA.US"));
    });
}
