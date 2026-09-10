use std::{env, process};

use crate::ui::git_status::*;

#[test]
fn status_z_parses_plain_untracked_and_rename() {
    let raw = b" M src/main.rs\0?? new file.txt\0R  new-name.rs\0old-name.rs\0A  a\xC3\xA9.txt\0";
    let entries = parse_status_z(raw);
    assert_eq!(
        entries,
        vec![
            (" M".to_string(), "src/main.rs".to_string()),
            ("??".to_string(), "new file.txt".to_string()),
            ("R ".to_string(), "new-name.rs".to_string()),
            ("A ".to_string(), "aé.txt".to_string()),
        ]
    );
}

#[test]
fn numstat_z_parses_counts_binary_and_rename() {
    let raw = b"3\t1\tsrc/lib.rs\0-\t-\tassets/logo.png\x005\t0\t\0old.rs\0new.rs\0";
    let entries = parse_numstat_z(raw);
    assert_eq!(
        entries,
        vec![
            ("src/lib.rs".to_string(), 3, 1),
            ("assets/logo.png".to_string(), 0, 0),
            ("new.rs".to_string(), 5, 0),
        ]
    );
}

#[test]
fn numstat_z_handles_unicode_paths() {
    let raw = "2\t0\tdocs/héllo wörld.md\0".as_bytes();
    assert_eq!(
        parse_numstat_z(raw),
        vec![("docs/héllo wörld.md".to_string(), 2, 0)]
    );
}

#[test]
fn diff_hides_headers_and_tracks_both_line_numbers() {
    let text = "diff --git a/f b/f\nindex 123..456 100644\n--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n context\n-removed\n+added\n";
    let rows = parse_diff(text);
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].kind, DiffLineKind::Hunk);
    assert_eq!(
        (rows[1].old_line, rows[1].new_line, rows[1].text.as_ref()),
        (Some(1), Some(1), "context")
    );
    assert_eq!(
        (rows[2].old_line, rows[2].new_line, rows[2].text.as_ref()),
        (Some(2), None, "removed")
    );
    assert_eq!(
        (rows[3].old_line, rows[3].new_line, rows[3].text.as_ref()),
        (None, Some(2), "added")
    );
}

#[test]
fn diff_truncates_past_cap() {
    let count = MAX_DIFF_LINES + 10;
    let text = format!("@@ -0,0 +1,{count} @@\n{}", "+x\n".repeat(count));
    let lines = parse_diff(&text);
    assert_eq!(lines.len(), MAX_DIFF_LINES + 1);
    assert_eq!(lines.last().unwrap().kind, DiffLineKind::Truncated);
}

#[test]
fn count_lines_handles_trailing_newline_and_binary() {
    let dir = env::temp_dir().join(format!("nmt-count-lines-{}", process::id()));
    fs::create_dir_all(&dir).unwrap();
    let root = dir.to_string_lossy().to_string();

    let check = |name: &str, bytes: &[u8], expected: u64| {
        fs::write(dir.join(name), bytes).unwrap();
        assert_eq!(count_file_lines(&root, name), expected, "{name}");
    };

    check("empty", b"", 0);
    check("trailing", b"one\ntwo\n", 2);
    check("no-trailing", b"one\ntwo", 2);
    check("binary", b"bin\0ary", 0);
    assert_eq!(count_file_lines(&root, "missing"), 0);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn diff_resets_numbers_and_keeps_header_like_code() {
    let rows = parse_diff(
        "@@ -7 +9 @@\n---code\n+++code\n\\ No newline at end of file\n@@ -30,0 +32,2 @@\n+first\n+second\n@@ -40,2 +43,0 @@\n-old\n-last\n",
    );
    let content: Vec<_> = rows
        .iter()
        .filter(|r| r.old_line.is_some() || r.new_line.is_some())
        .map(|r| (r.old_line, r.new_line, r.text.as_ref()))
        .collect();
    assert_eq!(
        content,
        vec![
            (Some(7), None, "--code"),
            (None, Some(9), "++code"),
            (None, Some(32), "first"),
            (None, Some(33), "second"),
            (Some(40), None, "old"),
            (Some(41), None, "last"),
        ]
    );
    assert_eq!(rows[3].kind, DiffLineKind::Notice);
    assert_eq!((rows[3].old_line, rows[3].new_line), (None, None));
}

#[test]
fn diff_preserves_binary_notice_without_file_metadata() {
    let rows = parse_diff(
        "diff --git a/image b/image\nindex a..b 100644\nBinary files a/image and b/image differ\n",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, DiffLineKind::Notice);
    assert!(!rows[0].text.contains("diff --git"));
    assert!(
        parse_diff("diff --git a/a b/b\nsimilarity index 100%\nrename from a\nrename to b\n")
            .is_empty()
    );
}

#[test]
fn untracked_diff_has_new_line_numbers_without_added_prefix() {
    let dir = env::temp_dir().join(format!("nmt-untracked-diff-{}", process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("new.txt"), "first\n+second").unwrap();
    let rows = fetch_file_diff(dir.to_str().unwrap(), "new.txt", true);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (rows[0].old_line, rows[0].new_line, rows[0].text.as_ref()),
        (None, Some(1), "first")
    );
    assert_eq!(
        (rows[1].old_line, rows[1].new_line, rows[1].text.as_ref()),
        (None, Some(2), "+second")
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn diff_handles_windows_line_endings_without_changing_source_numbers() {
    let rows = parse_diff(
        "diff --git a/f b/f\r\n--- a/f\r\n+++ b/f\r\n@@ -20,2 +20,2 @@\r\n unchanged\r\n-old\r\n+new\r\n",
    );
    assert_eq!(rows.len(), 4);
    assert_eq!((rows[1].old_line, rows[1].new_line), (Some(20), Some(20)));
    assert_eq!((rows[2].old_line, rows[2].new_line), (Some(21), None));
    assert_eq!((rows[3].old_line, rows[3].new_line), (None, Some(21)));
    assert_eq!(rows[3].text.as_ref(), "new");
}
