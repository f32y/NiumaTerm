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
fn diff_lines_classify_by_prefix() {
    let text = "diff --git a/f b/f\nindex 123..456 100644\n--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n context\n-removed\n+added\n";
    let kinds: Vec<DiffLineKind> = parse_diff(text).iter().map(|l| l.kind).collect();
    assert_eq!(
        kinds,
        vec![
            DiffLineKind::FileHeader,
            DiffLineKind::FileHeader,
            DiffLineKind::FileHeader,
            DiffLineKind::FileHeader,
            DiffLineKind::Hunk,
            DiffLineKind::Context,
            DiffLineKind::Removed,
            DiffLineKind::Added,
        ]
    );
}

#[test]
fn diff_truncates_past_cap() {
    let text = "+x\n".repeat(MAX_DIFF_LINES + 10);
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
