use std::collections::HashSet;

use crate::unix::macos::login_shell::{capture, importable, marker, parse};

const MARKER: &str = "__MARKER__";

fn dump(entries: &[&str]) -> String {
    format!("{MARKER}\n{}", entries.join("\0"))
}

fn owned(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

#[test]
fn parse_skips_startup_output_before_the_marker() {
    let output = format!(
        "Welcome to your shell\ncompinit: insecure directories\n{}",
        dump(&["PATH=/opt/bin", "HOME=/Users/someone"])
    );

    assert_eq!(
        parse(output.as_bytes(), MARKER),
        owned(&[("PATH", "/opt/bin"), ("HOME", "/Users/someone")])
    );
}

/// A shell function exported into the environment spans several lines, which
/// is why the dump is separated by NUL rather than by newline.
#[test]
fn parse_keeps_values_containing_newlines_and_equals_signs() {
    let output = dump(&["BODY=first\nsecond", "OPTIONS=a=1,b=2"]);

    assert_eq!(
        parse(output.as_bytes(), MARKER),
        owned(&[("BODY", "first\nsecond"), ("OPTIONS", "a=1,b=2")])
    );
}

#[test]
fn parse_yields_nothing_when_the_shell_never_reached_the_marker() {
    assert!(parse(b"login: no shell\n", MARKER).is_empty());
}

#[test]
fn importable_replaces_path_and_adds_only_what_is_missing() {
    let captured = owned(&[
        ("PATH", "/Users/someone/.local/bin:/usr/bin"),
        ("HOME", "/Users/someone"),
        ("PNPM_HOME", "/Users/someone/Library/pnpm"),
        ("PWD", "/Users/someone"),
    ]);

    let already_set: HashSet<&str> = ["PATH", "HOME"].into_iter().collect();

    assert_eq!(
        importable(captured, |name| already_set.contains(name)),
        owned(&[
            ("PATH", "/Users/someone/.local/bin:/usr/bin"),
            ("PNPM_HOME", "/Users/someone/Library/pnpm"),
        ])
    );
}

/// A startup file that echoes a name it was asked about, or an exported
/// variable whose value is quoted startup output, would let a fixed marker
/// appear ahead of the dump and cut the parse short.
#[test]
fn each_run_gets_a_marker_of_its_own() {
    assert_ne!(marker(), marker());
}

#[test]
fn capture_reads_a_path_from_a_real_shell() {
    let path = capture("/bin/sh")
        .expect("the shell produced a dump")
        .into_iter()
        .find(|(name, _)| name == "PATH")
        .expect("the dump carries PATH");

    assert!(!path.1.is_empty());
}

#[test]
fn capture_reports_a_shell_that_cannot_be_run() {
    assert!(capture("/nonexistent/shell").is_none());
}
