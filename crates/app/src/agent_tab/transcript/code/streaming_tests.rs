use std::cell::Cell;
use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, Context, Entity, TestAppContext, rgb};
use gpui_component::highlighter::HighlightTheme;
use nmt_agent::chat::Item;
use nmt_agent::transcript::TextField;
use nmt_config::profile::AgentKind;

use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::transcript::TranscriptView;
use crate::agent_tab::transcript::code::CodeView;
use crate::agent_tab::transcript::code::parser::CodeParser;
use crate::agent_tab::transcript::code::prepared::transcript_segments;
use crate::agent_tab::transcript::code::render_cache::CodeRenderCache;
use crate::agent_tab::transcript::code::source::CodeSource;
use crate::agent_tab::transcript::code::tests::register_languages;

#[derive(Clone, Copy, Debug, Default)]
struct WorkCounts {
    copied: usize,
    normalized: usize,
    indexed: usize,
    syntax_updates: usize,
    syntax_bytes: usize,
    style_queries: usize,
}

thread_local! {
    static WORK: Cell<WorkCounts> = Cell::default();
}

pub(super) fn record_copied(bytes: usize) {
    WORK.update(|mut work| {
        work.copied += bytes;

        work
    });
}

pub(super) fn record_normalized(bytes: usize) {
    WORK.update(|mut work| {
        work.normalized += bytes;

        work
    });
}

pub(super) fn record_indexed(bytes: usize) {
    WORK.update(|mut work| {
        work.indexed += bytes;

        work
    });
}

pub(super) fn record_syntax(bytes: usize) {
    WORK.update(|mut work| {
        work.syntax_updates += 1;
        work.syntax_bytes += bytes;

        work
    });
}

pub(super) fn record_styles() {
    WORK.update(|mut work| {
        work.style_queries += 1;

        work
    });
}

fn ensure_output(view: &mut TranscriptView, cx: &mut Context<TranscriptView>) -> Entity<CodeView> {
    let conversation = view.conversation.clone();
    let conversation = conversation.borrow();

    view.code_transcripts
        .ensure(0, &conversation.content.entries()[0].item, cx)
}

fn finish_parse(cx: &mut TestAppContext) {
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(30));
    cx.run_until_parked();
}

#[gpui::test]
fn streamed_output_processes_each_byte_once(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    WORK.set(WorkCounts::default());

    let initial = "first line\n".repeat(2048);
    let delta = "more output\n".repeat(128);

    let view = cx.update(|cx| {
        cx.new(|cx| {
            let mut view = TranscriptView::new(AgentKind::Codex, None);

            view.push(
                1,
                Item::CommandExecution {
                    id: "stream".into(),
                    command: "echo".into(),
                    purpose: None,
                    aggregated_output: Some(initial.clone()),
                    status: Some("inProgress".into()),
                    exit_code: None,
                },
                Vec::new(),
                cx,
            );

            view
        })
    });

    let output = view.update(cx, ensure_output);

    finish_parse(cx);

    for _ in 0..32 {
        view.update(cx, |view, cx| {
            view.append_delta("stream", &delta, TextField::CommandOutput);

            ensure_output(view, cx);
        });

        finish_parse(cx);
    }

    let bytes = initial.len() + delta.len() * 32;
    let work = WORK.get();

    assert_eq!(work.copied, bytes, "{work:?}");
    assert_eq!(work.normalized, bytes, "{work:?}");
    assert!(
        work.indexed <= bytes + 4096 * 34,
        "{work:?}; output bytes: {bytes}"
    );

    output.read_with(cx, |view, _| {
        assert_eq!(
            view.prepared.as_ref().unwrap().text.to_string(),
            format!("$ echo\n\n{initial}{}", delta.repeat(32))
        );
    });
}

fn source(output: &str) -> CodeSource {
    CodeSource {
        command: Some("cat code.rs".into()),
        output: output.into(),
        language: Some("rs".into()),
        strip_gutter: false,
    }
}

#[test]
fn split_ansi_and_unicode_match_complete_output_at_each_update() {
    let raw =
        "\x1b[31m红\r\n\x1b[0mplain\rprogress\x1b[38;2;12;34;56m🎉\x1b[0m\x1b]52;c;ignored\x07\n";

    let mut parser = CodeParser::new(source(""));
    let mut end = 0;

    for ch in raw.chars() {
        let start = end;

        end += ch.len_utf8();

        parser.append(&raw[start..end]);

        let complete = CodeParser::new(source(&raw[..end]));
        let current = &parser.prepared;

        assert_eq!(
            current.text.to_string(),
            complete.prepared.text.to_string(),
            "split at {end}"
        );
        assert_eq!(
            current
                .segments
                .iter()
                .map(|row| row.range.clone())
                .collect::<Vec<_>>(),
            transcript_segments(&current.text.to_string()),
            "split at {end}"
        );
        assert_eq!(
            current.styles(
                0..current.text.len(),
                &HighlightTheme::default_dark(),
                rgb(0xffffff).into(),
                rgb(0).into()
            ),
            complete.prepared.styles(
                0..complete.prepared.text.len(),
                &HighlightTheme::default_dark(),
                rgb(0xffffff).into(),
                rgb(0).into()
            ),
            "split at {end}"
        );
    }
}

#[test]
fn long_lines_reuse_completed_segments_and_preserve_old_snapshots() {
    let mut parser = CodeParser::new(source(&format!("{}\n", "x".repeat(20000))));

    let previous = parser.prepared.clone();
    let first = previous.segments.get(2).unwrap().text.clone();

    for delta in [
        "\x1b[32m",
        &"界".repeat(3000),
        "\n\n",
        "tail",
        "\r",
        "\nnext",
    ] {
        parser.append(delta);
    }

    let text = parser.prepared.text.to_string();
    let expected = transcript_segments(&text);

    assert_eq!(parser.prepared.segments.len(), expected.len());

    for (index, range) in expected.iter().enumerate() {
        let row = parser.prepared.segments.get(index).unwrap();

        assert_eq!(&row.range, range);
        assert_eq!(row.text.as_str(), &text[range.clone()]);
    }

    assert_eq!(
        parser.prepared.segments.get(2).unwrap().text.as_ptr(),
        first.as_ptr()
    );
    assert_eq!(
        previous.text.to_string(),
        format!("$ cat code.rs\n\n{}\n", "x".repeat(20000))
    );
    assert_eq!(
        parser.prepared.widest_segment,
        expected
            .iter()
            .enumerate()
            .max_by_key(|(_, range)| range.len())
            .unwrap()
            .0
    );
}

#[test]
fn incremental_syntax_matches_full_parse_and_preserves_displayed_snapshots() {
    register_languages();

    let mut parser = CodeParser::new(source(""));

    parser.prepared.parse_syntax();

    let mut output = String::new();

    let theme = HighlightTheme::default_dark();

    for delta in [
        "/* comment",
        "\ncontinued */\n",
        "fn main() {\n",
        "let text = \"hello",
        "界\";\n",
        "}\n",
    ] {
        let previous = parser.prepared.clone();

        let old_styles = previous.styles(
            0..previous.text.len(),
            &theme,
            rgb(0xffffff).into(),
            rgb(0).into(),
        );

        output.push_str(delta);
        WORK.set(WorkCounts::default());
        parser.append(delta);
        parser.prepared.parse_syntax();

        let work = WORK.get();

        assert_eq!(work.syntax_updates, 1, "{work:?}");
        assert_eq!(work.syntax_bytes, delta.len(), "{work:?}");

        let mut complete = CodeParser::new(source(&output));

        complete.prepared.parse_syntax();

        let text = &parser.prepared.text;

        assert_eq!(
            parser
                .prepared
                .styles(0..text.len(), &theme, rgb(0xffffff).into(), rgb(0).into()),
            complete
                .prepared
                .styles(0..text.len(), &theme, rgb(0xffffff).into(), rgb(0).into())
        );
        assert_eq!(
            previous.styles(
                0..previous.text.len(),
                &theme,
                rgb(0xffffff).into(),
                rgb(0).into()
            ),
            old_styles
        );
    }
}

#[test]
fn repeated_row_render_reuses_styles_and_theme_changes_refresh_them() {
    register_languages();

    let mut parser = CodeParser::new(source("let value = 42;\n"));

    parser.prepared.parse_syntax();

    let prepared = Arc::new(parser.prepared.clone());
    let dark = HighlightTheme::default_dark();
    let light = HighlightTheme::default_light();

    let mut cache = CodeRenderCache::default();

    WORK.set(WorkCounts::default());

    let range = prepared.segments.get(2).unwrap().range.clone();

    let mut dark_styles = Vec::new();

    for _ in 0..10 {
        cache.update(&prepared, &dark, rgb(0xffffff).into(), rgb(0).into());

        dark_styles = cache.styles(2, range.clone());
    }

    assert_eq!(WORK.get().style_queries, 1);

    cache.update(&prepared, &light, rgb(0xffffff).into(), rgb(0).into());

    assert_ne!(cache.styles(2, range.clone()), dark_styles);
    assert_eq!(WORK.get().style_queries, 2);

    parser.append("let next = true;\n");
    parser.prepared.parse_syntax();

    cache.update(
        &Arc::new(parser.prepared),
        &light,
        rgb(0xffffff).into(),
        rgb(0).into(),
    );

    cache.styles(2, range);

    assert_eq!(WORK.get().style_queries, 3);
}

#[gpui::test]
fn virtual_output_redraws_do_not_query_styles_again(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);

        cx.set_global(AgentSettings::default());
    });

    register_languages();

    WORK.set(WorkCounts::default());

    let (view, cx) =
        cx.add_window_view(|_, cx| CodeView::new(source(&"let value = 42;\n".repeat(300)), cx));

    finish_parse(cx);

    cx.update(|window, cx| window.draw(cx).clear(cx));

    assert!(view.read_with(cx, |view, _| view.prepared.as_ref().unwrap().virtualized));

    let queries = WORK.get().style_queries;

    assert!(queries > 0);

    for _ in 0..5 {
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    assert_eq!(WORK.get().style_queries, queries);
}

#[gpui::test]
fn unrelated_text_and_status_updates_leave_tool_output_cached(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);

    let view = cx.update(|cx| {
        cx.new(|cx| {
            let mut view = TranscriptView::new(AgentKind::Codex, None);

            view.push(
                1,
                Item::AgentMessage {
                    id: "reply".into(),
                    text: Some("before".into()),
                    questions: None,
                },
                Vec::new(),
                cx,
            );

            view.push(
                1,
                Item::CommandExecution {
                    id: "tool".into(),
                    command: "echo".into(),
                    purpose: None,
                    aggregated_output: Some("output\n".repeat(4096)),
                    status: Some("inProgress".into()),
                    exit_code: None,
                },
                Vec::new(),
                cx,
            );

            view
        })
    });

    let ensure = |view: &mut TranscriptView, cx: &mut Context<TranscriptView>| {
        let conversation = view.conversation.clone();
        let conversation = conversation.borrow();

        view.code_transcripts
            .ensure(1, &conversation.content.entries()[1].item, cx)
    };

    let code = view.update(cx, ensure);

    finish_parse(cx);

    let before = code.read_with(cx, |code, _| code.prepared.clone().unwrap());

    WORK.set(WorkCounts::default());

    view.update(cx, |view, cx| {
        view.append_delta("reply", " more", TextField::Reply);

        ensure(view, cx);

        view.merge_completed(&Item::CommandExecution {
            id: "tool".into(),
            command: "echo".into(),
            purpose: None,
            aggregated_output: None,
            status: Some("completed".into()),
            exit_code: Some(0),
        });

        ensure(view, cx);
    });

    finish_parse(cx);

    assert_eq!(WORK.get().copied, 0);
    assert_eq!(WORK.get().normalized, 0);

    code.read_with(cx, |code, _| {
        assert!(Arc::ptr_eq(code.prepared.as_ref().unwrap(), &before))
    });
}
