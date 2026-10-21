use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::{AppContext as _, TestAppContext};
use markdown::mdast::Node;

use crate::text::MarkdownExtensions;
use crate::text::state::{
    ParseMode, ParsedContent, ParsedUpdate, TextViewState, UpdateOptions, parse_content,
};

fn completed_parse(state: &TextViewState) -> ParsedUpdate {
    ParsedUpdate {
        revision: state.revision,
        full_parse: true,
        selection_compatible: false,
        baseline_ack: false,
        result: parse_content(
            state.format,
            ParsedContent::default(),
            &UpdateOptions {
                revision: state.revision,
                pending_text: state.text.clone(),
                append: false,
                mode: ParseMode::Replace,
                markdown_extensions: state.markdown_extensions.clone(),
            },
        ),
    }
}

#[gpui::test]
fn streamed_set_text_keeps_completed_markdown_blocks(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let headings = Arc::new(AtomicUsize::new(0));
    let parsed_headings = headings.clone();
    let extensions = MarkdownExtensions::default().block_parser(move |node, _| {
        if matches!(node, Node::Heading(_)) {
            parsed_headings.fetch_add(1, Ordering::Relaxed);
        }
        None
    });
    let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown("# Stable\n\nTail", cx)));
    state.update(cx, |state, cx| {
        state.set_markdown_extensions(Arc::new(extensions), cx);
    });
    cx.run_until_parked();
    headings.store(0, Ordering::Relaxed);

    for text in ["# Stable\n\nTail one", "# Stable\n\nTail one two"] {
        state.update(cx, |state, cx| state.set_text(text, cx));
        cx.run_until_parked();
        state.read_with(cx, |state, _| assert_eq!(state.source().as_str(), text));
    }
    assert_eq!(
        headings.load(Ordering::Relaxed),
        0,
        "appending to the final paragraph must reuse completed blocks"
    );
}

#[gpui::test]
fn completed_prefix_is_displayed_while_newer_text_is_pending(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let initial = "A complete paragraph.\n\n".repeat(250);
    let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown(&initial, cx)));
    let completed = state.read_with(cx, |state, _| completed_parse(state));
    let latest = format!("{initial}New text still being parsed.");

    state.update(cx, |state, cx| {
        state.set_text(&latest, cx);
        assert!(state.apply_parsed_update(completed, cx));
        assert_eq!(state.source().as_str(), initial);
    });
    cx.run_until_parked();
    state.read_with(cx, |state, _| assert_eq!(state.source().as_str(), latest));
}

#[gpui::test]
fn replacement_rejects_old_results_even_when_it_reuses_the_same_prefix(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let initial = "A complete paragraph.\n\n".repeat(250);
    let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown(&initial, cx)));
    let completed = state.read_with(cx, |state, _| completed_parse(state));
    let replacement = format!("{initial}A different response.");

    state.update(cx, |state, cx| {
        state.set_text("Replacement started.", cx);
        state.set_text(&replacement, cx);
        assert!(!state.apply_parsed_update(completed, cx));
    });
    cx.run_until_parked();
    state.read_with(cx, |state, _| {
        assert_eq!(state.source().as_str(), replacement)
    });
}

#[gpui::test]
fn parser_change_rejects_pending_results_from_the_previous_configuration(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let text = "A complete paragraph.\n\n".repeat(250);
    let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown(&text, cx)));
    let completed = state.read_with(cx, |state, _| completed_parse(state));
    state.update(cx, |state, cx| {
        state.set_markdown_extensions(
            Arc::new(MarkdownExtensions::default().block_parser(|_, _| None)),
            cx,
        );
        assert!(!state.apply_parsed_update(completed, cx));
    });
    cx.run_until_parked();
    state.read_with(cx, |state, _| assert_eq!(state.source().as_str(), text));
}

#[gpui::test]
fn streamed_unicode_and_code_fences_preserve_complete_source(cx: &mut TestAppContext) {
    cx.update(crate::init);
    let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown("# Title\n\n", cx)));
    let mut text = String::from("# Title\n\n");
    for delta in [
        "```rust\n",
        "let message = \"",
        "\u{4f60}\u{597d}",
        "\";\n",
        "```\n\n",
        "Done.",
    ] {
        text.push_str(delta);
        state.update(cx, |state, cx| state.set_text(&text, cx));
        cx.run_until_parked();
        state.read_with(cx, |state, _| assert_eq!(state.source().as_str(), text));
    }
    state.update(cx, |state, cx| {
        state.select_all(cx);
        assert!(
            state
                .selected_text()
                .contains("let message = \"\u{4f60}\u{597d}\";")
        );
        assert!(state.selected_text().trim_end().ends_with("Done."));
    });
}

#[gpui::test]
fn streamed_reference_definitions_match_a_complete_markdown_parse(cx: &mut TestAppContext) {
    cx.update(crate::init);
    for (initial, delta) in [
        (
            "Read [guide].\n\nLater.",
            "\n\n[guide]: https://example.com",
        ),
        (
            "[guide]: https://example.com\n\nFirst.\n\n",
            "Read [guide].",
        ),
        ("Read[^note].\n\nLater.", "\n\n[^note]: Details."),
    ] {
        let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown(initial, cx)));
        cx.run_until_parked();
        state.update(cx, |state, cx| {
            state.set_text(&format!("{initial}{delta}"), cx)
        });
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            let expected = completed_parse(state).result.unwrap();
            assert_eq!(
                state.parsed_content.document.blocks,
                expected.document.blocks
            );
        });
    }
}
