use std::ops::Range;
use std::sync::Once;
use std::time::Duration;

use gpui::{AppContext as _, HighlightStyle, Hsla, TestAppContext, rgb};
use gpui_component::highlighter::{HighlightTheme, LanguageConfig, LanguageRegistry};
use nmt_agent_utils::chat::Item;
use vte::Parser;

use crate::transcript::code::CodeView;
use crate::transcript::code::ansi::{AnsiColor, AnsiText};
use crate::transcript::code::prepared::PreparedCode;
use crate::transcript::code::source::CodeSource;
use crate::transcript::{detect_output_language, entry_copy_text};

fn register_languages() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        for (name, language, query) in [
            (
                "bash",
                tree_sitter_bash::LANGUAGE,
                tree_sitter_bash::HIGHLIGHT_QUERY,
            ),
            (
                "powershell",
                tree_sitter_powershell::LANGUAGE,
                tree_sitter_powershell::HIGHLIGHTS_QUERY,
            ),
            (
                "diff",
                tree_sitter_diff::LANGUAGE,
                tree_sitter_diff::HIGHLIGHTS_QUERY,
            ),
            (
                "rs",
                tree_sitter_rust::LANGUAGE,
                tree_sitter_rust::HIGHLIGHTS_QUERY,
            ),
        ] {
            LanguageRegistry::singleton().register(
                name,
                &LanguageConfig::new(name, language.into(), vec![], query, "", ""),
            );
        }
    });
}

fn command(command: &str, output: &str) -> Item {
    Item::CommandExecution {
        id: "command".into(),
        command: command.into(),
        purpose: None,
        aggregated_output: Some(output.into()),
        status: Some("completed".into()),
        exit_code: Some(0),
    }
}

fn prepare(item: &Item) -> PreparedCode {
    register_languages();
    let mut result = PreparedCode::new(&CodeSource::from_item(item).unwrap());
    result.parse_syntax();
    result
}

fn styles(
    prepared: &PreparedCode,
    range: Range<usize>,
    theme: &HighlightTheme,
) -> Vec<(Range<usize>, HighlightStyle)> {
    prepared.styles(range, theme, rgb(0xeeeeee).into(), rgb(0x202020).into())
}

fn color_at(styles: &[(Range<usize>, HighlightStyle)], at: usize) -> Option<Hsla> {
    styles
        .iter()
        .find(|(range, _)| range.contains(&at))
        .and_then(|(_, style)| style.color)
}

#[test]
fn bash_command_and_json_output_receive_independent_highlights() {
    let prepared = prepare(&command("echo \"$HOME\"", "{\"count\":42}"));
    assert_eq!(prepared.text.as_str(), "$ echo \"$HOME\"\n\n{\"count\":42}");
    let theme = HighlightTheme::default_dark();
    let spans = styles(&prepared, 0..prepared.text.len(), &theme);
    assert!(color_at(&spans, prepared.text.find("HOME").unwrap()).is_some());
    assert_eq!(
        color_at(&spans, prepared.text.find("42").unwrap()),
        theme.style("number").unwrap().color
    );
}

#[test]
fn powershell_script_arguments_are_parsed_inside_the_launcher_quotes() {
    for source in [
        "Write-Output \"hello\"",
        "pwsh -NoProfile -Command 'Write-Output \"hello\"'",
    ] {
        let prepared = prepare(&command(source, "done"));
        let theme = HighlightTheme::default_dark();
        let spans = styles(&prepared, 0..prepared.text.len(), &theme);
        assert_eq!(
            color_at(&spans, prepared.text.find("hello").unwrap()),
            theme.style("string").unwrap().color,
            "{source}"
        );
    }
}

#[test]
fn literal_file_reads_color_source_but_transformed_commands_do_not_guess() {
    for source in [
        "cat 'src/my file.rs'",
        "Get-Content -Raw -LiteralPath 'src/my file.rs'",
    ] {
        let prepared = prepare(&command(source, "fn main() {}"));
        let spans = styles(
            &prepared,
            0..prepared.text.len(),
            &HighlightTheme::default_dark(),
        );
        assert!(color_at(&spans, prepared.text.find("fn main").unwrap()).is_some());
    }
    for source in [
        "cat one.rs two.rs",
        "cat src/main.rs | wc -l",
        "cat $path",
        "Get-Content a.rs -TotalCount 3",
        "cat -n a.rs",
    ] {
        assert_eq!(
            CodeSource::from_item(&command(source, "result"))
                .unwrap()
                .language,
            None,
            "{source}"
        );
    }
    let item = Item::Other {
        id: "read".into(),
        kind: "Read".into(),
        title: "main.rs".into(),
        output: Some("  1→fn main() {}".into()),
        status: None,
    };
    let prepared = prepare(&item);
    assert_eq!(prepared.text.as_str(), "fn main() {}\n");
    assert!(
        color_at(
            &styles(
                &prepared,
                0..prepared.text.len(),
                &HighlightTheme::default_dark()
            ),
            0
        )
        .is_some()
    );
}

#[test]
fn output_detection_accepts_streaming_json_and_unified_diffs_without_coloring_log_levels() {
    for source in [
        "[INFO] started",
        "[ERROR] failed",
        "{not json}",
        "normal output",
    ] {
        assert_eq!(detect_output_language(source), "");
    }
    for source in ["{\"result\":", "[1,", "[true, false]", "[ {\"a\":1} ]"] {
        assert_eq!(detect_output_language(source), "json");
    }
    let prepared = prepare(&command(
        "git diff",
        "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new",
    ));
    let spans = styles(
        &prepared,
        0..prepared.text.len(),
        &HighlightTheme::default_dark(),
    );
    let removed = color_at(&spans, prepared.text.find("-old").unwrap());
    let added = color_at(&spans, prepared.text.find("+new").unwrap());
    assert!(removed.is_some() && added.is_some());
    assert_ne!(removed, added);
}

#[test]
fn ansi_state_survives_chunk_boundaries_and_removes_nontext_controls() {
    let raw = "\x1b[31m红\nline\x1b[0m plain\r\n\x1b]52;c;ignored\x07**literal**\x1b[2K";
    let mut output = AnsiText::default();
    let mut parser = Parser::new();
    for byte in raw.as_bytes() {
        parser.advance(&mut output, &[*byte]);
    }
    assert_eq!(output.text, "红\nline plain\n**literal**");
    assert_eq!(output.spans.len(), 1);
    assert_eq!(output.spans[0].0, 0.."红\nline".len());
    assert_eq!(output.spans[0].1.foreground, Some(AnsiColor::Indexed(1)));
    assert_eq!(AnsiText::parse(raw).spans, output.spans);
}

#[test]
fn ansi_extended_colors_and_resets_preserve_valid_ranges() {
    let output =
        AnsiText::parse("\x1b[38;5;196mA\x1b[38;2;12;34;56mB\x1b[38:2::65:43:21mC\x1b[0mD");
    assert_eq!(output.text, "ABCD");
    assert_eq!(
        output
            .spans
            .iter()
            .map(|(_, style)| style.foreground)
            .collect::<Vec<_>>(),
        vec![
            Some(AnsiColor::Indexed(196)),
            Some(AnsiColor::Rgb(12, 34, 56)),
            Some(AnsiColor::Rgb(65, 43, 21))
        ]
    );
    let malformed = AnsiText::parse("\x1b[38;2;999;31;255mplain");
    assert!(malformed.spans.is_empty());
    let partial = AnsiText::parse("plain\x1b[38;2;");
    assert_eq!(partial.text, "plain");
}

#[test]
fn ansi_color_overrides_syntax_and_copy_contains_only_displayed_text() {
    let item = command("echo json", "{\"count\":\x1b[38;2;12;34;56m42\x1b[0m}");
    let prepared = prepare(&item);
    let spans = styles(
        &prepared,
        0..prepared.text.len(),
        &HighlightTheme::default_dark(),
    );
    assert_eq!(
        color_at(&spans, prepared.text.find("42").unwrap()),
        Some(rgb(0x0c2238).into())
    );
    assert_eq!(entry_copy_text(&item), prepared.text.as_str());
}

#[test]
fn virtual_rows_keep_highlights_at_distant_offsets_and_remap_theme_colors() {
    let output = format!(
        "[\n{}{{\"value\":900}}\n]",
        "{\"value\":123},\n".repeat(900)
    );
    let prepared = prepare(&command("echo data", &output));
    assert!(prepared.virtualized);
    let at = prepared.text.rfind("900").unwrap();
    let row = prepared
        .segments
        .iter()
        .find(|range| range.contains(&at))
        .unwrap()
        .clone();
    let dark = styles(&prepared, row.clone(), &HighlightTheme::default_dark());
    let light = styles(&prepared, row.clone(), &HighlightTheme::default_light());
    assert!(color_at(&dark, at - row.start).is_some());
    assert_ne!(
        color_at(&dark, at - row.start),
        color_at(&light, at - row.start)
    );
    assert!(dark.iter().all(|(span, _)| span.end <= row.len()));
}

#[gpui::test]
fn latest_stream_revision_replaces_same_length_content_and_finishes_pending_ansi(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    register_languages();
    let first = CodeSource::from_item(&command("echo", "old\x1b[31")).unwrap();
    let view = cx.update(|cx| cx.new(|cx| CodeView::new(first, cx)));
    view.update(cx, |view, cx| {
        view.set_source(
            CodeSource::from_item(&command("echo", "new\x1b[31")).unwrap(),
            cx,
        )
    });
    view.update(cx, |view, cx| {
        view.set_source(
            CodeSource::from_item(&command("echo", "new\x1b[31m red")).unwrap(),
            cx,
        )
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(30));
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert_eq!(
            view.prepared.as_ref().unwrap().text.as_str(),
            "$ echo\n\nnew red"
        );
        assert!(view.parse_task.is_none());
    });
}
