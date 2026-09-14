use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const CLEAN: &str = "fn run() {\n    let value = 1;\n\n    consume(value);\n}\n";
const CROWDED: &str = "fn run() {\n    let value = 1;\n    consume(value);\n}\n";
const SOURCE: &str = "crates/demo/src/lib.rs";

#[test]
fn optional_spacing_rules_require_explicit_enablement_in_all_modes() {
    let repo = Repository::new();
    let source = r#"enum Value {
    /// A documented variant.
    Documented,
    #[error("failed")]
    Failed,
    Data {
        value: u8,
    },
    Empty,
}

fn run(value: u8) {
    match value {
        0 => {
            first();
        }
        _ => second(),
    }
}
"#;

    repo.baseline("");
    repo.write(SOURCE, source);
    repo.git(&["add", SOURCE]);

    for mode in ["--check", "--fix", "--staged"] {
        let result = repo.tool(&[mode]);

        assert_eq!(result.status.code(), Some(0), "{mode}: {result:?}");
        assert_eq!(
            fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
            source
        );
    }

    for (rule, other, expected) in [
        (
            "spacing/match-arms",
            "spacing/enum-variants",
            source.replace("        }\n        _", "        }\n\n        _"),
        ),
        (
            "spacing/enum-variants",
            "spacing/match-arms",
            source
                .replace("    Documented,\n", "    Documented,\n\n")
                .replace("    Failed,\n", "    Failed,\n\n")
                .replace("    },\n", "    },\n\n"),
        ),
    ] {
        for mode in ["--check", "--staged"] {
            let result = repo.tool(&[mode, "--enable", rule]);
            let output = String::from_utf8_lossy(&result.stdout);

            assert_eq!(result.status.code(), Some(1), "{mode}: {result:?}");
            assert!(output.contains(rule), "{output}");
            assert!(!output.contains(other), "{output}");
        }

        let fixed = repo.tool(&["--fix", "--enable", rule, SOURCE]);

        assert_eq!(fixed.status.code(), Some(0), "{fixed:?}");
        assert_eq!(
            fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
            expected
        );
        assert_eq!(repo.git(&["show", &format!(":{SOURCE}")]), source);

        let checked = repo.tool(&["--check", "--enable", rule, SOURCE]);

        assert_eq!(checked.status.code(), Some(0), "{checked:?}");

        repo.write(SOURCE, source);
    }

    let both = repo.tool(&[
        "--check",
        "--enable",
        "spacing/match-arms",
        "--enable",
        "spacing/enum-variants",
        SOURCE,
    ]);

    let output = String::from_utf8_lossy(&both.stdout);

    assert_eq!(both.status.code(), Some(1), "{both:?}");
    assert!(output.contains("spacing/match-arms"));
    assert!(output.contains("spacing/enum-variants"));
}

#[test]
fn rejects_missing_or_unknown_optional_rules_before_fixing_files() {
    let repo = Repository::new();

    repo.baseline(CROWDED);

    for arguments in [
        vec!["--fix", SOURCE, "--enable"],
        vec!["--fix", SOURCE, "--enable", "spacing/match-arm"],
    ] {
        let result = repo.tool(&arguments);

        assert_eq!(result.status.code(), Some(2), "{result:?}");
        assert_eq!(
            fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
            CROWDED
        );
    }
}

#[test]
fn rejects_and_removes_blank_lines_between_arms_and_variants_using_the_index() {
    for (clean, boundary, rule) in [
        (
            "enum Value {\n    First,\n    Second,\n    Third,\n}\n",
            "    First,\n",
            "spacing/enum-variant-blank-lines",
        ),
        (
            "fn run() {\n    match value {\n        0 => first(),\n        1 => second(),\n        _ => third(),\n    }\n}\n",
            "        0 => first(),\n",
            "spacing/match-arm-blank-lines",
        ),
    ] {
        let repo = Repository::new();
        let spaced = clean.replace(boundary, &format!("{boundary}\n\n"));

        repo.baseline(clean);
        repo.write(SOURCE, &spaced);
        repo.git(&["add", SOURCE]);
        repo.write(SOURCE, clean);

        let staged = repo.tool(&["--staged"]);

        assert_eq!(staged.status.code(), Some(1), "{staged:?}");
        assert!(String::from_utf8_lossy(&staged.stdout).contains(rule));
        assert!(String::from_utf8_lossy(&staged.stdout).contains("unexpected blank line"));

        let checked = repo.tool(&["--check", SOURCE]);

        assert_eq!(checked.status.code(), Some(0), "{checked:?}");

        repo.write(SOURCE, &spaced);

        let checked = repo.tool(&["--check", SOURCE]);

        assert_eq!(checked.status.code(), Some(1), "{checked:?}");
        assert!(String::from_utf8_lossy(&checked.stdout).contains(rule));

        for _ in 0..2 {
            let fixed = repo.tool(&["--fix", SOURCE]);

            assert_eq!(fixed.status.code(), Some(0), "{fixed:?}");
            assert_eq!(
                fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
                clean
            );
            assert_eq!(repo.git(&["show", &format!(":{SOURCE}")]), spaced);
        }

        repo.baseline(&spaced);
        repo.write(
            SOURCE,
            &spaced.replace("Third", "Last").replace("third()", "last()"),
        );
        repo.git(&["add", SOURCE]);

        let staged = repo.tool(&["--staged"]);
        let full = repo.tool(&["--check", SOURCE]);

        assert_eq!(staged.status.code(), Some(0), "{staged:?}");
        assert_eq!(full.status.code(), Some(1), "{full:?}");
        assert!(String::from_utf8_lossy(&full.stdout).contains(rule));
    }
}

#[test]
fn separates_binding_mutability_without_hiding_staged_spacing_deletions() {
    let repo = Repository::new();
    let source = "fn run() {\n    let transcript = read();\n    let mut tasks: Vec<Task> = Vec::new();\n    let mut index = HashMap::new();\n    let ready = true;\n}\n";
    let clean = source
        .replace("read();\n", "read();\n\n")
        .replace("HashMap::new();\n", "HashMap::new();\n\n");

    repo.baseline(&clean);
    repo.write(SOURCE, source);
    repo.git(&["add", SOURCE]);

    let checked = repo.tool(&["--check", SOURCE]);
    let output = String::from_utf8_lossy(&checked.stdout).replace('\\', "/");

    assert_eq!(checked.status.code(), Some(1), "{checked:?}");
    assert!(output.contains("crates/demo/src/lib.rs:3:1: spacing/binding-mutability"));
    assert!(output.contains("crates/demo/src/lib.rs:5:1: spacing/binding-mutability"));

    let fixed = repo.tool(&["--fix", SOURCE]);

    assert_eq!(fixed.status.code(), Some(0), "{fixed:?}");
    assert_eq!(
        fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
        clean
    );
    assert_eq!(repo.git(&["show", &format!(":{SOURCE}")]), source);

    let checked = repo.tool(&["--check", SOURCE]);
    let staged = repo.tool(&["--staged"]);
    let output = String::from_utf8_lossy(&staged.stdout).replace('\\', "/");

    assert_eq!(checked.status.code(), Some(0), "{checked:?}");
    assert_eq!(staged.status.code(), Some(1), "{staged:?}");
    assert!(output.contains("crates/demo/src/lib.rs:3:1: spacing/binding-mutability"));
    assert!(output.contains("crates/demo/src/lib.rs:5:1: spacing/binding-mutability"));

    repo.git(&["add", SOURCE]);

    let staged = repo.tool(&["--staged"]);

    assert_eq!(staged.status.code(), Some(0), "{staged:?}");
}

#[test]
fn checks_and_fixes_call_and_assertion_spacing_without_changing_the_index() {
    let repo = Repository::new();
    let source = "fn run() {\n    merge_update(&mut summary, &update, sequence);\n    self.merge_update(&mut summary, &update, sequence);\n    self.tasks.insert(key, summary);\n    self.activity += 1;\n    assert!(self.activity > 0);\n    debug_assert_eq!(self.tasks.len(), 1);\n    finish();\n}\n";
    let clean = source
        .replacen("&update, sequence);\n", "&update, sequence);\n\n", 1)
        .replace("insert(key, summary);\n", "insert(key, summary);\n\n")
        .replace("self.activity += 1;\n", "self.activity += 1;\n\n")
        .replace(
            "debug_assert_eq!(self.tasks.len(), 1);\n",
            "debug_assert_eq!(self.tasks.len(), 1);\n\n",
        );

    repo.baseline(&clean);
    repo.write(SOURCE, source);
    repo.git(&["add", SOURCE]);
    repo.write(SOURCE, &clean);

    let staged = repo.tool(&["--staged"]);
    let output = String::from_utf8_lossy(&staged.stdout).replace('\\', "/");

    assert_eq!(staged.status.code(), Some(1), "{staged:?}");
    assert!(output.contains("crates/demo/src/lib.rs:3:1: spacing/call-and-statement"));
    assert!(!output.contains("crates/demo/src/lib.rs:4:1: spacing/call-and-statement"));
    assert!(output.contains("crates/demo/src/lib.rs:5:1: spacing/call-and-statement"));
    assert!(output.contains("crates/demo/src/lib.rs:6:1: spacing/assertions"));
    assert!(output.contains("crates/demo/src/lib.rs:8:1: spacing/assertions"));

    repo.write(SOURCE, source);

    let checked = repo.tool(&["--check", SOURCE]);

    assert_eq!(checked.status.code(), Some(1), "{checked:?}");
    assert!(String::from_utf8_lossy(&checked.stdout).contains("spacing/call-and-statement"));
    assert!(String::from_utf8_lossy(&checked.stdout).contains("spacing/assertions"));

    let fixed = repo.tool(&["--fix", SOURCE]);

    assert_eq!(fixed.status.code(), Some(0), "{fixed:?}");
    assert_eq!(
        fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
        clean
    );
    assert_eq!(repo.git(&["show", &format!(":{SOURCE}")]), source);

    repo.git(&["add", SOURCE]);
    repo.write(SOURCE, source);

    let staged = repo.tool(&["--staged"]);

    assert_eq!(staged.status.code(), Some(0), "{staged:?}");
}

#[test]
fn rejects_immediate_closure_calls_without_rewriting_them_and_checks_staged_changes() {
    let repo = Repository::new();
    let source = "fn run() -> Result<(), String> {\n    let result = (|| -> Result<(), String> {\n        write()?;\n\n        Ok(())\n    })();\n\n    result\n}\n\nconst VERSION: u8 = 1;\n";
    let clean = source.replace(
        "(|| -> Result<(), String> {\n        write()?;\n\n        Ok(())\n    })()",
        "write()",
    );

    repo.baseline("");
    repo.write(SOURCE, source);
    repo.git(&["add", SOURCE]);
    repo.write(SOURCE, &clean);

    let staged = repo.tool(&["--staged"]);

    assert_eq!(staged.status.code(), Some(1), "{staged:?}");
    assert!(String::from_utf8_lossy(&staged.stdout).contains("expressions/immediate-closure-call"));

    let checked = repo.tool(&["--check", SOURCE]);

    assert_eq!(checked.status.code(), Some(0), "{checked:?}");

    repo.write(SOURCE, source);

    for mode in ["--check", "--fix"] {
        let result = repo.tool(&[mode, SOURCE]);

        assert_eq!(result.status.code(), Some(1), "{mode}: {result:?}");
        assert!(
            String::from_utf8_lossy(&result.stdout).contains("expressions/immediate-closure-call")
        );
        assert_eq!(
            fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
            source
        );
        assert_eq!(repo.git(&["show", &format!(":{SOURCE}")]), source);
    }

    repo.git(&["commit", "-qm", "existing closure call"]);
    repo.write(
        SOURCE,
        &source.replace("VERSION: u8 = 1", "VERSION: u8 = 2"),
    );
    repo.git(&["add", SOURCE]);

    let staged = repo.tool(&["--staged"]);
    let full = repo.tool(&["--check", SOURCE]);

    assert_eq!(staged.status.code(), Some(0), "{staged:?}");
    assert_eq!(full.status.code(), Some(1), "{full:?}");
    assert!(String::from_utf8_lossy(&full.stdout).contains("expressions/immediate-closure-call"));

    repo.write(SOURCE, &source.replace("write()?", "flush()?"));
    repo.git(&["add", SOURCE]);

    let staged = repo.tool(&["--staged"]);

    assert_eq!(staged.status.code(), Some(1), "{staged:?}");
    assert!(String::from_utf8_lossy(&staged.stdout).contains("expressions/immediate-closure-call"));
}

#[test]
fn fixed_option_returns_require_explicit_enablement_in_all_modes() {
    let rule = "expressions/fixed-option-return";

    for value in ["Some(1)", "None"] {
        let repo = Repository::new();
        let clean = "fn value() -> Option<u8> {\n    lookup()\n}\n\nconst VERSION: u8 = 1;\n";
        let source = clean.replace("lookup()", value);

        repo.baseline(clean);
        repo.write(SOURCE, &source);
        repo.git(&["add", SOURCE]);

        for mode in ["--check", "--fix", "--staged"] {
            let result = repo.tool(&[mode]);

            assert_eq!(result.status.code(), Some(0), "{mode}: {result:?}");
            assert!(!String::from_utf8_lossy(&result.stdout).contains(rule));
            assert_eq!(
                fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
                source
            );
            assert_eq!(repo.git(&["show", &format!(":{SOURCE}")]), source);
        }

        repo.write(SOURCE, clean);

        let staged = repo.tool(&["--staged", "--enable", rule]);

        assert_eq!(staged.status.code(), Some(1), "{staged:?}");
        assert!(
            String::from_utf8_lossy(&staged.stdout).contains("expressions/fixed-option-return")
        );

        let checked = repo.tool(&["--check", "--enable", rule, SOURCE]);

        assert_eq!(checked.status.code(), Some(0), "{checked:?}");

        repo.write(SOURCE, &source);

        for mode in ["--check", "--fix"] {
            let result = repo.tool(&[mode, "--enable", rule, SOURCE]);

            assert_eq!(result.status.code(), Some(1), "{mode}: {result:?}");
            assert!(
                String::from_utf8_lossy(&result.stdout).contains("expressions/fixed-option-return")
            );
            assert_eq!(
                fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
                source
            );
            assert_eq!(repo.git(&["show", &format!(":{SOURCE}")]), source);
        }

        repo.git(&["commit", "-qm", "existing fixed return"]);
        repo.write(
            SOURCE,
            &source.replace("VERSION: u8 = 1", "VERSION: u8 = 2"),
        );
        repo.git(&["add", SOURCE]);

        let staged = repo.tool(&["--staged", "--enable", rule]);
        let full = repo.tool(&["--check", "--enable", rule, SOURCE]);

        assert_eq!(staged.status.code(), Some(0), "{staged:?}");
        assert_eq!(full.status.code(), Some(1), "{full:?}");
        assert!(String::from_utf8_lossy(&full.stdout).contains("expressions/fixed-option-return"));
    }
}

struct Repository(TempDir);

impl Repository {
    fn new() -> Self {
        let repo = Self(tempfile::tempdir().unwrap());

        repo.git(&["init", "-q"]);
        repo.git(&["config", "user.email", "test@example.com"]);
        repo.git(&["config", "user.name", "ReadabilityTest"]);
        repo.git(&["config", "core.autocrlf", "false"]);
        repo.git(&["config", "core.hooksPath", ".git/no-hooks"]);
        repo.git(&["config", "commit.gpgsign", "false"]);

        repo
    }

    fn write(&self, path: &str, source: &str) {
        let path = self.0.path().join(path);

        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }

    fn git(&self, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(self.0.path())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .args(arguments)
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        String::from_utf8(output.stdout).unwrap()
    }

    fn tool(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_nmt-readability"))
            .current_dir(self.0.path())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .args(arguments)
            .output()
            .unwrap()
    }

    fn baseline(&self, source: &str) {
        self.write(SOURCE, source);
        self.git(&["add", "."]);
        self.git(&["commit", "-qm", "baseline"]);
    }
}

#[test]
fn staged_check_reads_the_index_in_both_partial_staging_directions() {
    let repo = Repository::new();

    repo.baseline(CLEAN);
    repo.write(SOURCE, CROWDED);
    repo.git(&["add", SOURCE]);
    repo.write(SOURCE, CLEAN);

    let result = repo.tool(&["--staged"]);
    let output = String::from_utf8_lossy(&result.stdout).replace('\\', "/");

    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert!(output.contains("crates/demo/src/lib.rs:3:1: spacing/binding-and-action"));
    assert_eq!(repo.git(&["show", &format!(":{SOURCE}")]), CROWDED);
    assert_eq!(
        fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
        CLEAN
    );

    repo.write(SOURCE, &CLEAN.replace('1', "2"));
    repo.git(&["add", SOURCE]);
    repo.write(SOURCE, CROWDED);

    let result = repo.tool(&["--staged"]);

    assert_eq!(result.status.code(), Some(0), "{result:?}");
}

#[test]
fn skips_old_issues_outside_the_changed_boundaries() {
    let repo = Repository::new();
    let baseline = format!("{CROWDED}\nconst VALUE: u8 = 1;\n");

    repo.baseline(&baseline);
    repo.write(SOURCE, &baseline.replace("VALUE: u8 = 1", "VALUE: u8 = 2"));
    repo.git(&["add", SOURCE]);

    let staged = repo.tool(&["--staged"]);
    let full = repo.tool(&["--check"]);

    assert_eq!(staged.status.code(), Some(0), "{staged:?}");
    assert_eq!(full.status.code(), Some(1), "{full:?}");
}

#[test]
fn catches_deleted_spacing_after_comments_despite_git_display_settings() {
    let repo = Repository::new();
    let baseline = "fn run() {\n    let value = 1;\n    // The value is ready for processing.\n\n    consume(value);\n}\n";

    repo.baseline(baseline);
    repo.git(&["config", "color.ui", "always"]);
    repo.git(&["config", "diff.interHunkContext", "100"]);
    repo.write(".gitattributes", "*.rs -diff\n");

    repo.write(
        SOURCE,
        &baseline.replace("processing.\n\n", "processing.\n"),
    );

    repo.git(&["add", "."]);

    let result = repo.tool(&["--staged"]);

    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert!(String::from_utf8_lossy(&result.stdout).contains("spacing/binding-and-action"));
}

#[test]
fn default_check_includes_untracked_sources_but_excludes_ignored_files() {
    let repo = Repository::new();

    repo.baseline(CLEAN);
    repo.write(".gitignore", "crates/demo/generated/\n");
    repo.write("crates/demo/generated/output.rs", "not valid Rust");
    repo.write("crates/demo/src/new.rs", CROWDED);

    let result = repo.tool(&["--check"]);

    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert!(String::from_utf8_lossy(&result.stdout).contains("checked 2 Rust file(s)"));
}

#[test]
fn fixes_working_files_without_changing_the_index_and_is_idempotent() {
    let repo = Repository::new();

    repo.baseline(CROWDED);

    let fixed = repo.tool(&["--fix", "crates/demo/src"]);

    assert_eq!(fixed.status.code(), Some(0), "{fixed:?}");
    assert_eq!(
        fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
        CLEAN
    );
    assert_eq!(repo.git(&["show", &format!(":{SOURCE}")]), CROWDED);

    let again = repo.tool(&["--fix", SOURCE]);

    assert_eq!(again.status.code(), Some(0), "{again:?}");
    assert!(String::from_utf8_lossy(&again.stdout).contains("0 spacing issue(s)"));
}

#[test]
fn handles_new_repositories_spaces_renames_and_deletions() {
    let repo = Repository::new();
    let path = "crates/demo/src/a \u{3bb} file.rs";

    repo.write(path, CROWDED);
    repo.git(&["add", "."]);

    let added = repo.tool(&["--staged"]);

    assert_eq!(added.status.code(), Some(1), "{added:?}");
    assert!(String::from_utf8_lossy(&added.stdout).contains("a \u{3bb} file.rs:3:1:"));

    repo.git(&["commit", "-qm", "baseline"]);
    repo.git(&["mv", path, SOURCE]);

    let renamed = repo.tool(&["--staged"]);

    assert_eq!(renamed.status.code(), Some(1), "{renamed:?}");

    repo.git(&["commit", "-qm", "rename"]);
    repo.git(&["rm", SOURCE]);

    let deleted = repo.tool(&["--staged"]);

    assert_eq!(deleted.status.code(), Some(0), "{deleted:?}");
}

#[test]
fn excludes_other_sources_and_reports_invalid_staged_rust() {
    let repo = Repository::new();

    repo.baseline(CLEAN);
    repo.write("third_party/demo.rs", CROWDED);
    repo.write("crates/demo/script.js", "this is not Rust");
    repo.git(&["add", "."]);

    let other = repo.tool(&["--staged"]);

    assert_eq!(other.status.code(), Some(0), "{other:?}");

    repo.write(SOURCE, "fn broken( {");
    repo.git(&["add", SOURCE]);
    repo.write(SOURCE, CLEAN);

    let invalid = repo.tool(&["--staged"]);

    assert_eq!(invalid.status.code(), Some(2), "{invalid:?}");
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("lib.rs"));
}

#[test]
fn rejects_conflicting_modes_without_writing() {
    let repo = Repository::new();

    repo.baseline(CROWDED);

    for arguments in [
        ["--staged", "--fix"],
        ["--fix", "--staged"],
        ["--check", "missing.rs"],
    ] {
        let result = repo.tool(&arguments);

        assert_eq!(result.status.code(), Some(2), "{result:?}");
    }

    assert_eq!(
        fs::read_to_string(repo.0.path().join(Path::new(SOURCE))).unwrap(),
        CROWDED
    );
}

#[test]
fn checks_declaration_groups_and_fixes_only_their_spacing() {
    let repo = Repository::new();
    let source = "pub use crate::api::Public;\npub(crate) use crate::api::Internal;\npub(super) use crate::api::Parent;\npub mod api;\npub(crate) mod shared;\npub(super) mod sibling;\nmod private;\n#[cfg(test)]\nmod tests;\nuse std::fmt;\n";
    let expected = source.replace(";\n", ";\n\n");

    repo.write(SOURCE, source);

    let checked = repo.tool(&["--check", SOURCE]);

    assert_eq!(checked.status.code(), Some(1), "{checked:?}");
    assert!(String::from_utf8_lossy(&checked.stdout).contains("spacing/declaration-groups"));

    let fixed = repo.tool(&["--fix", SOURCE]);

    assert_eq!(fixed.status.code(), Some(0), "{fixed:?}");
    assert_eq!(
        fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
        expected.trim_end_matches('\n').to_owned() + "\n"
    );

    repo.write(SOURCE, "use std::fmt;\n\npub mod api;\n");

    let out_of_order = repo.tool(&["--fix", SOURCE]);

    assert_eq!(out_of_order.status.code(), Some(1), "{out_of_order:?}");
    assert!(String::from_utf8_lossy(&out_of_order.stdout).contains("declarations/order"));
    assert_eq!(
        fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
        "use std::fmt;\n\npub mod api;\n"
    );

    let same_line = "pub use std::fmt; use std::io;\n";

    repo.write(SOURCE, same_line);

    let mixed = repo.tool(&["--fix", SOURCE]);

    assert_eq!(mixed.status.code(), Some(1), "{mixed:?}");
    assert!(String::from_utf8_lossy(&mixed.stdout).contains("declarations/group-spacing"));
    assert_eq!(
        fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
        same_line
    );
}

#[test]
fn checks_module_headers_and_visibility_but_exempts_local_order() {
    let repo = Repository::new();

    for (source, rule) in [
        ("fn run() {}\n\nuse std::fmt;\n", "declarations/header"),
        (
            "mod outer {\n    const N: u8 = 0;\n\n    mod inner;\n}\n",
            "declarations/header",
        ),
        ("pub(self) use std::fmt;\n", "declarations/visibility"),
        ("pub(in crate) mod inner;\n", "declarations/visibility"),
    ] {
        repo.write(SOURCE, source);

        let result = repo.tool(&["--check", SOURCE]);

        assert_eq!(result.status.code(), Some(1), "{source}: {result:?}");
        assert!(String::from_utf8_lossy(&result.stdout).contains(rule));
    }

    repo.write(
        SOURCE,
        "fn run() {\n    action();\n\n    use std::fmt;\n\n    mod local { fn run() {} use std::io; }\n}\n",
    );

    let local = repo.tool(&["--check", SOURCE]);

    assert_eq!(local.status.code(), Some(0), "{local:?}");
}

#[test]
fn staged_visibility_checks_report_changed_methods_without_rewriting_them() {
    let repo = Repository::new();
    let source = "impl Value {\n    pub(crate) fn run(&self) {\n        first();\n\n        second();\n    }\n}\n";
    let restricted = source.replace("pub(crate)", "pub(in crate::outer)");

    repo.baseline(source);
    repo.write(SOURCE, &restricted);
    repo.git(&["add", SOURCE]);
    repo.write(SOURCE, source);

    let staged = repo.tool(&["--staged"]);
    let output = String::from_utf8_lossy(&staged.stdout).replace('\\', "/");

    assert_eq!(staged.status.code(), Some(1), "{staged:?}");
    assert!(output.contains("crates/demo/src/lib.rs:2:5: declarations/visibility"));

    repo.write(SOURCE, &restricted);

    let fixed = repo.tool(&["--fix", SOURCE]);

    assert_eq!(fixed.status.code(), Some(1), "{fixed:?}");
    assert_eq!(
        fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
        restricted
    );

    repo.git(&["commit", "-qm", "existing method visibility"]);
    repo.write(SOURCE, &restricted.replace("second()", "third()"));
    repo.git(&["add", SOURCE]);

    let staged = repo.tool(&["--staged"]);
    let full = repo.tool(&["--check", SOURCE]);

    assert_eq!(staged.status.code(), Some(0), "{staged:?}");
    assert_eq!(full.status.code(), Some(1), "{full:?}");
    assert!(String::from_utf8_lossy(&full.stdout).contains("declarations/visibility"));
}

#[test]
fn staged_declaration_checks_follow_both_changed_items_and_the_index() {
    let repo = Repository::new();

    repo.baseline("pub use std::io;\n\npub(crate) use std::fmt;\n");
    repo.write(SOURCE, "use std::io;\n\npub(crate) use std::fmt;\n");
    repo.git(&["add", SOURCE]);
    repo.write(SOURCE, "pub use std::fmt;\n\nuse std::io;\n");

    let order = repo.tool(&["--staged"]);

    assert_eq!(order.status.code(), Some(1), "{order:?}");
    assert!(String::from_utf8_lossy(&order.stdout).contains("declarations/order"));

    repo.git(&["add", SOURCE]);
    repo.git(&["commit", "-qm", "ordered declarations"]);
    repo.write(SOURCE, "fn run() {}\n\npub use std::fmt;\n\nuse std::io;\n");
    repo.git(&["add", SOURCE]);

    let header = repo.tool(&["--staged"]);

    assert_eq!(header.status.code(), Some(1), "{header:?}");
    assert!(String::from_utf8_lossy(&header.stdout).contains("declarations/header"));
}

#[test]
fn staged_checks_detect_deleted_declaration_group_spacing() {
    let repo = Repository::new();
    let source = "pub use std::fmt;\n\npub(crate) use std::io;\n";

    repo.baseline(source);
    repo.write(SOURCE, &source.replace(";\n\n", ";\n"));
    repo.git(&["add", SOURCE]);

    let result = repo.tool(&["--staged"]);

    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert!(String::from_utf8_lossy(&result.stdout).contains("spacing/declaration-groups"));
}

#[test]
fn staged_checks_leave_existing_declaration_issues_outside_changes_alone() {
    let repo = Repository::new();

    let source =
        "fn run() {\n    first();\n\n    second();\n}\n\nuse std::fmt;\n\nconst N: u8 = 1;\n";

    repo.baseline(source);
    repo.write(SOURCE, &source.replace("second()", "third()"));
    repo.git(&["add", SOURCE]);

    let staged = repo.tool(&["--staged"]);
    let full = repo.tool(&["--check", SOURCE]);

    assert_eq!(staged.status.code(), Some(0), "{staged:?}");
    assert_eq!(full.status.code(), Some(1), "{full:?}");
    assert!(String::from_utf8_lossy(&full.stdout).contains("declarations/header"));

    let source = "pub(crate) mod inner {\n    fn run() {\n        first();\n\n        second();\n    }\n}\n\npub use std::fmt;\n";

    repo.write(SOURCE, source);
    repo.git(&["add", SOURCE]);
    repo.git(&["commit", "-qm", "existing module order issue"]);
    repo.write(SOURCE, &source.replace("second()", "third()"));
    repo.git(&["add", SOURCE]);

    let staged = repo.tool(&["--staged"]);
    let full = repo.tool(&["--check", SOURCE]);

    assert_eq!(staged.status.code(), Some(0), "{staged:?}");
    assert_eq!(full.status.code(), Some(1), "{full:?}");
    assert!(String::from_utf8_lossy(&full.stdout).contains("declarations/order"));
}

#[test]
fn staged_checks_detect_removed_test_cfg_without_rewriting_modules() {
    let repo = Repository::new();
    let source = "#[cfg(test)]\nmod parser_tests;\n";
    let missing = "mod parser_tests;\n";

    repo.baseline(source);
    repo.write(SOURCE, missing);
    repo.git(&["add", SOURCE]);
    repo.write(SOURCE, source);

    let staged = repo.tool(&["--staged"]);

    assert_eq!(staged.status.code(), Some(1), "{staged:?}");
    assert!(String::from_utf8_lossy(&staged.stdout).contains("declarations/test-module-cfg"));

    repo.write(SOURCE, missing);

    let fixed = repo.tool(&["--fix", SOURCE]);

    assert_eq!(fixed.status.code(), Some(1), "{fixed:?}");
    assert_eq!(
        fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
        missing
    );
}

#[test]
fn fixes_module_doc_spacing_without_hiding_a_staged_deletion() {
    let repo = Repository::new();
    let source = "//! Module details.\n\nuse std::fmt;\n";
    let crowded = source.replace("details.\n\n", "details.\n");

    repo.baseline(source);
    repo.write(SOURCE, &crowded);
    repo.git(&["add", SOURCE]);
    repo.write(SOURCE, source);

    let staged = repo.tool(&["--staged"]);

    assert_eq!(staged.status.code(), Some(1), "{staged:?}");
    assert!(String::from_utf8_lossy(&staged.stdout).contains("spacing/module-docs"));

    repo.write(SOURCE, &crowded);

    let fixed = repo.tool(&["--fix", SOURCE]);

    assert_eq!(fixed.status.code(), Some(0), "{fixed:?}");
    assert_eq!(
        fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
        source
    );
    assert_eq!(repo.git(&["show", &format!(":{SOURCE}")]), crowded);
}

#[test]
fn rejects_changed_path_attributes_without_rewriting_them() {
    let repo = Repository::new();
    let source = "mod inner;\n\nfn run() {\n    first();\n}\n";
    let mapped = format!("#[path = \"other.rs\"]\n{source}");

    repo.baseline(source);
    repo.write(SOURCE, &mapped);
    repo.git(&["add", SOURCE]);
    repo.write(SOURCE, source);

    let staged = repo.tool(&["--staged"]);

    assert_eq!(staged.status.code(), Some(1), "{staged:?}");
    assert!(String::from_utf8_lossy(&staged.stdout).contains("declarations/path-attribute"));

    repo.write(SOURCE, &mapped);

    let fixed = repo.tool(&["--fix", SOURCE]);

    assert_eq!(fixed.status.code(), Some(1), "{fixed:?}");
    assert_eq!(
        fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
        mapped
    );

    repo.git(&["commit", "-qm", "existing module path"]);
    repo.write(SOURCE, &mapped.replace("first()", "second()"));
    repo.git(&["add", SOURCE]);

    let unrelated = repo.tool(&["--staged"]);
    let full = repo.tool(&["--check", SOURCE]);

    assert_eq!(unrelated.status.code(), Some(0), "{unrelated:?}");
    assert_eq!(full.status.code(), Some(1), "{full:?}");
}

#[test]
fn staged_module_paths_require_test_cfg_even_when_attributes_separate_them() {
    let repo = Repository::new();
    let source = "#[cfg(test)]\n#[allow(dead_code)]\n#[allow(unused_imports)]\n#[path = \"fixtures.rs\"]\nmod fixtures;\n";

    repo.baseline(source);

    let fixed = repo.tool(&["--fix", SOURCE]);

    assert_eq!(fixed.status.code(), Some(0), "{fixed:?}");
    assert_eq!(
        fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
        source
    );

    repo.write(SOURCE, &source.replace("#[cfg(test)]\n", ""));
    repo.git(&["add", SOURCE]);
    repo.write(SOURCE, source);

    let staged = repo.tool(&["--staged"]);
    let full = repo.tool(&["--check", SOURCE]);

    assert_eq!(staged.status.code(), Some(1), "{staged:?}");
    assert!(String::from_utf8_lossy(&staged.stdout).contains("declarations/path-attribute"));
    assert_eq!(full.status.code(), Some(0), "{full:?}");

    repo.git(&["add", SOURCE]);

    let staged = repo.tool(&["--staged"]);

    assert_eq!(staged.status.code(), Some(0), "{staged:?}");
}

#[test]
fn fixes_import_spacing_without_hiding_deleted_spacing_in_the_index() {
    let repo = Repository::new();
    let source = "use std::fmt;\n\nuse serde::Serialize;\n\nuse crate::api::Public;\n";
    let crowded = source.replace(";\n\n", ";\n");

    repo.baseline(source);
    repo.write(SOURCE, &crowded);
    repo.git(&["add", SOURCE]);

    let fixed = repo.tool(&["--fix", SOURCE]);

    assert_eq!(fixed.status.code(), Some(0), "{fixed:?}");
    assert_eq!(
        fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
        source
    );
    assert_eq!(repo.git(&["show", &format!(":{SOURCE}")]), crowded);

    let staged = repo.tool(&["--staged"]);

    assert_eq!(staged.status.code(), Some(1), "{staged:?}");
    assert!(String::from_utf8_lossy(&staged.stdout).contains("spacing/import-groups"));
}

#[test]
fn staged_import_checks_follow_both_changed_imports_and_ignore_old_issues() {
    for (baseline, changed, rule) in [
        (
            "use std::fmt;\n\nuse serde::Serialize;\n\nuse crate::api::Public;\n",
            "use crate::api::Other;\n\nuse serde::Serialize;\n\nuse crate::api::Public;\n",
            "import-order",
        ),
        (
            "use serde::Serialize;\n\nuse crate::api::Public;\n",
            "use serde::Serialize;\n\nuse std::fmt;\n",
            "import-order",
        ),
        (
            "use {anyhow::Result, serde::Serialize};\n",
            "use {serde::Serialize, crate::api::Public};\n",
            "mixed-imports",
        ),
    ] {
        let repo = Repository::new();
        let baseline = format!("{baseline}\n{CLEAN}");
        let changed = format!("{changed}\n{CLEAN}");

        repo.baseline(&baseline);
        repo.write(SOURCE, &changed);
        repo.git(&["add", SOURCE]);
        repo.write(SOURCE, &baseline);

        let staged = repo.tool(&["--staged"]);

        assert_eq!(staged.status.code(), Some(1), "{staged:?}");
        assert!(String::from_utf8_lossy(&staged.stdout).contains(rule));

        repo.write(SOURCE, &changed);

        let fixed = repo.tool(&["--fix", SOURCE]);

        assert_eq!(fixed.status.code(), Some(1), "{fixed:?}");
        assert_eq!(
            fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
            changed
        );

        repo.git(&["commit", "-qm", "existing import issue"]);
        repo.write(SOURCE, &changed.replace("value = 1", "value = 2"));
        repo.git(&["add", SOURCE]);

        let staged = repo.tool(&["--staged"]);
        let full = repo.tool(&["--check", SOURCE]);

        assert_eq!(staged.status.code(), Some(0), "{staged:?}");
        assert_eq!(full.status.code(), Some(1), "{full:?}");
        assert!(String::from_utf8_lossy(&full.stdout).contains(rule));
    }
}

#[test]
fn staged_private_inline_module_checks_follow_the_header_and_exclude_the_body() {
    let repo = Repository::new();

    let source =
        "#[allow(dead_code)]\n#[allow(unused_imports)]\n#[cfg(unix)]\nmod sys;\n\nuse std::fmt;\n";

    let changed = source.replace(
        "mod sys;",
        "mod sys {\n    fn run() {\n        first();\n\n        second();\n    }\n}",
    );

    repo.baseline(source);
    repo.write(SOURCE, &changed);
    repo.git(&["add", SOURCE]);
    repo.write(SOURCE, source);

    let staged = repo.tool(&["--staged"]);

    assert_eq!(staged.status.code(), Some(1), "{staged:?}");
    assert!(String::from_utf8_lossy(&staged.stdout).contains("declarations/header"));

    repo.git(&["commit", "-qm", "existing private module placement"]);
    repo.write(SOURCE, &changed.replace("second()", "third()"));
    repo.git(&["add", SOURCE]);

    let staged = repo.tool(&["--staged"]);
    let full = repo.tool(&["--check", SOURCE]);

    assert_eq!(staged.status.code(), Some(0), "{staged:?}");
    assert_eq!(full.status.code(), Some(1), "{full:?}");
    assert!(String::from_utf8_lossy(&full.stdout).contains("declarations/header"));
}

#[test]
fn staged_alphabetical_checks_follow_both_imports_and_preserve_manual_fixes() {
    for (ordered, reversed) in [
        (
            "use crate::alpha::Value;\n\nuse crate::middle::Value;\n",
            "use crate::zeta::Value;\n\nuse crate::middle::Value;\n",
        ),
        (
            "use crate::middle::Value;\n\nuse crate::zeta::Value;\n",
            "use crate::middle::Value;\n\nuse crate::alpha::Value;\n",
        ),
    ] {
        let repo = Repository::new();
        let ordered = format!("{ordered}\n{CLEAN}");
        let reversed = format!("{reversed}\n{CLEAN}");

        repo.baseline(&ordered);
        repo.write(SOURCE, &reversed);
        repo.git(&["add", SOURCE]);
        repo.write(SOURCE, &ordered);

        let staged = repo.tool(&["--staged"]);

        assert_eq!(staged.status.code(), Some(1), "{staged:?}");
        assert!(
            String::from_utf8_lossy(&staged.stdout).contains("declarations/import-alphabetical")
        );

        repo.write(SOURCE, &reversed);

        let fixed = repo.tool(&["--fix", SOURCE]);

        assert_eq!(fixed.status.code(), Some(1), "{fixed:?}");
        assert_eq!(
            fs::read_to_string(repo.0.path().join(SOURCE)).unwrap(),
            reversed
        );

        repo.git(&["commit", "-qm", "existing import alphabetical issue"]);
        repo.write(SOURCE, &reversed.replace("value = 1", "value = 2"));
        repo.git(&["add", SOURCE]);

        let staged = repo.tool(&["--staged"]);
        let full = repo.tool(&["--check", SOURCE]);

        assert_eq!(staged.status.code(), Some(0), "{staged:?}");
        assert_eq!(full.status.code(), Some(1), "{full:?}");
    }
}

#[test]
fn staged_nested_import_order_checks_use_only_the_affected_entries() {
    let repo = Repository::new();

    let ordered =
        "use crate::api::{\n    Alpha,\n    Zebra,\n    beta,\n    gamma,\n    omega,\n};\n";

    let reversed = ordered.replace("Alpha,\n    Zebra", "Zebra,\n    Alpha");

    repo.baseline(ordered);
    repo.write(SOURCE, &reversed);
    repo.git(&["add", SOURCE]);
    repo.write(SOURCE, ordered);

    let staged = repo.tool(&["--staged"]);
    let output = String::from_utf8_lossy(&staged.stdout).replace('\\', "/");

    assert_eq!(staged.status.code(), Some(1), "{staged:?}");
    assert!(output.contains("crates/demo/src/lib.rs:3:5: declarations/import-alphabetical"));

    repo.git(&["commit", "-qm", "existing nested import order"]);
    repo.write(SOURCE, &reversed.replace("omega,", "omega2,"));
    repo.git(&["add", SOURCE]);

    let staged = repo.tool(&["--staged"]);
    let full = repo.tool(&["--check", SOURCE]);

    assert_eq!(staged.status.code(), Some(0), "{staged:?}");
    assert_eq!(full.status.code(), Some(1), "{full:?}");
}
