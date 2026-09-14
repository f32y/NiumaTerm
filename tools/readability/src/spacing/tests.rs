use std::collections::BTreeMap;

use crate::spacing::{self, BlankLineEdit, Issue, Issues, Options, apply};

fn inspect(source: &str) -> Result<Issues, syn::Error> {
    Ok(spacing::inspect(
        source,
        &syn::parse_file(source)?,
        &Options::default(),
    ))
}

#[test]
fn separates_bindings_control_flow_and_results() {
    let source = "fn read() -> u8 {\n    let ready = true;\n    if !ready {\n        return 0;\n    }\n    consume();\n    1\n}\n";
    let issues = inspect(source).unwrap();

    assert_eq!(issues.len(), 3);
    assert_eq!(
        apply(source, &issues).unwrap(),
        "fn read() -> u8 {\n    let ready = true;\n\n    if !ready {\n        return 0;\n    }\n\n    consume();\n\n    1\n}\n"
    );
}

#[test]
fn separates_binding_mutability_in_both_directions() {
    let source = "fn run() {\n    let transcript = TranscriptIndex::read(reader);\n    let mut tasks: Vec<RestoredTask> = Vec::new();\n    let mut index: HashMap<String, usize> = HashMap::new();\n    let ready = true;\n    let count = 0;\n}\n";
    let expected = source
        .replace("read(reader);\n", "read(reader);\n\n")
        .replace("HashMap::new();\n", "HashMap::new();\n\n");
    let issues = inspect(source).unwrap();

    assert_eq!(issues.len(), 2);
    assert!(
        issues
            .values()
            .all(|issue| issue.rule == "binding-mutability")
    );
    assert_eq!(apply(source, &issues).unwrap(), expected);
    assert!(inspect(&expected).unwrap().is_empty());
}

#[test]
fn detects_mutable_bindings_inside_patterns() {
    for binding in [
        "let (left, mut right) = pair;",
        "let Pair { left, right: mut value } = pair;",
        "let Pair(left, mut right) = pair;",
        "let [first, mut second] = values;",
        "let ref mut value = input;",
        "let (mut value) = input;",
        "let whole @ (left, mut right) = pair;",
    ] {
        let source =
            format!("fn run() {{\n    let first = 1;\n    {binding}\n    let last = 2;\n}}\n");
        let expected = source
            .replace("let first = 1;\n", "let first = 1;\n\n")
            .replace(binding, &format!("{binding}\n"));
        let issues = inspect(&source).unwrap();

        assert_eq!(issues.len(), 2, "{source}");
        assert!(
            issues
                .values()
                .all(|issue| issue.rule == "binding-mutability")
        );
        assert_eq!(apply(&source, &issues).unwrap(), expected);
        assert!(inspect(&expected).unwrap().is_empty());
    }
}

#[test]
fn ignores_mutability_outside_binding_identifiers() {
    let source = "fn run() {\n    let first = 1;\n    let borrowed: &mut Value = &mut value;\n    let &mut copied = borrowed;\n    let callback = |mut input| input;\n    let future = async { let mut nested = 1; };\n}\n";

    assert!(inspect(source).unwrap().is_empty());
}

#[test]
fn separates_each_call_from_calls_and_assignments() {
    let source = "fn run() {\n    merge_update(&mut summary, &update, sequence);\n    self.tasks.insert(key, summary);\n    self.activity += 1;\n    self.ready = true;\n    flush()?;\n    (publish().await);\n}\n";

    let expected = source
        .replace("&update, sequence);\n", "&update, sequence);\n\n")
        .replace("insert(key, summary);\n", "insert(key, summary);\n\n")
        .replace("self.ready = true;\n", "self.ready = true;\n\n")
        .replace("flush()?;\n", "flush()?;\n\n");

    let issues = inspect(source).unwrap();

    assert_eq!(issues.len(), 4);
    assert!(
        issues
            .values()
            .all(|issue| issue.rule == "call-and-statement")
    );
    assert_eq!(apply(source, &issues).unwrap(), expected);
    assert!(inspect(&expected).unwrap().is_empty());
}

#[test]
fn separates_calls_from_other_statements_without_detaching_comments() {
    let source = "fn run() {\n    consume();\n    // Record progress after the operation finishes.\n    self.activity += 1;\n    trace!();\n    publish();\n}\n";

    let expected = source
        .replace("consume();\n", "consume();\n\n")
        .replace("trace!();\n", "trace!();\n\n");

    let issues = inspect(source).unwrap();

    assert_eq!(issues.len(), 2);
    assert!(
        issues
            .values()
            .all(|issue| issue.rule == "call-and-statement")
    );
    assert_eq!(apply(source, &issues).unwrap(), expected);
    assert!(inspect(&expected).unwrap().is_empty());
}

#[test]
fn separates_assertion_groups_from_other_statements() {
    let source = "fn run() {\n    prepare();\n    assert!(ready);\n    assert_eq!(value, 1);\n    debug_assert_ne!(value, 2);\n    self.checked = true;\n}\n";

    let expected = source.replace("prepare();\n", "prepare();\n\n").replace(
        "debug_assert_ne!(value, 2);\n",
        "debug_assert_ne!(value, 2);\n\n",
    );

    let issues = inspect(source).unwrap();

    assert_eq!(issues.len(), 2);
    assert!(issues.values().all(|issue| issue.rule == "assertions"));
    assert_eq!(apply(source, &issues).unwrap(), expected);
    assert!(inspect(&expected).unwrap().is_empty());
}

#[test]
fn keeps_multiline_and_tail_assertions_together() {
    for last in ["std::assert!(ready)", "(debug_assert!(ready))"] {
        let source = format!(
            "fn run() {{\n    assert_eq!(\n        actual,\n        expected,\n    );\n    debug_assert_ne!(actual, other);\n    {last}\n}}\n"
        );

        assert!(inspect(&source).unwrap().is_empty(), "{source}");
    }
}

#[test]
fn keeps_short_groups_and_single_line_items_compact() {
    let source = r#"use std::io;
use std::fmt;

const A: u8 = 1;
const B: u8 = 2;

fn run() {
    let a = 1;
    let b = 2;

    assert_eq!(a, 1);
    assert_eq!(b, 2);

    match a {
        1 => first(),
        2 => second(),
        _ => third(),
    }
}

mod compact { fn a() {} fn b() {} }
"#;

    assert!(inspect(source).unwrap().is_empty());
}

#[test]
fn inserts_before_comments_and_preserves_literal_contents() {
    let source = r##"fn run() {
    let message = r#"first
second
third"#;
    // The message includes its original line breaks.
    consume(message);
}
"##;

    let issues = inspect(source).unwrap();
    let fixed = apply(source, &issues).unwrap();

    assert_eq!(issues.len(), 1);
    assert!(fixed.contains("third\"#;\n\n    // The message"));
    assert!(fixed.contains("first\nsecond\nthird"));
    assert!(inspect(&fixed).unwrap().is_empty());
}

#[test]
fn leaves_trailing_multiline_comments_untouched() {
    let source = "fn run() {\n    let value = 1; /* keep\n    this text together */\n    consume(value);\n}\n";

    assert!(inspect(source).unwrap().is_empty());
}

#[test]
fn checks_fields_and_arm_bodies_without_requiring_match_arm_spacing() {
    let source = r#"struct Values {
    /// The first value.
    first: u8,
    /// The second value.
    second: u8,
}

fn run(value: u8) {
    match value {
        0 => {
            let value = 1;
            first(value);
        }
        _ => second(),
    }
}
"#;

    let issues = inspect(source).unwrap();

    assert_eq!(issues.len(), 2);
    assert!(
        issues
            .values()
            .any(|reasons| reasons.rule == "documented-fields")
    );
    assert!(!issues.values().any(|reasons| reasons.rule == "match-arms"));
    assert!(
        issues
            .values()
            .any(|reasons| reasons.rule == "binding-and-action")
    );
}

#[test]
fn respects_skipped_functions_methods_and_modules() {
    let source = r#"#[rustfmt::skip]
fn run() {
    let value = 1;
    consume(value);
}

impl Runner {
    #[rustfmt::skip]
    fn run() {
        let value = 1;
        consume(value);
    }
}

#[rustfmt::skip]
mod generated {
    fn run() {
        let value = 1;
        consume(value);
    }
}
"#;

    assert!(inspect(source).unwrap().is_empty());
}

#[test]
fn leaves_unknown_macro_input_opaque() {
    let source = r#"const SCRIPT: &str = stringify! {
    let value = 1;
    consume(value);
};
"#;

    assert!(inspect(source).unwrap().is_empty());
}

#[test]
fn checks_select_branches_and_documented_flags() {
    let source = r#"fn run() {
    tokio::select! {
        value = read() => {
            let value = value?;
            consume(value);
        }
        _ = stop() => { return; }
    }
}

bitflags! {
    struct Flags: u8 {
        const A = 1;
        /// The second flag.
        const B = 2;
    }
}
"#;

    let issues = inspect(source).unwrap();
    let fixed = apply(source, &issues).unwrap();

    assert_eq!(issues.len(), 3);
    assert!(inspect(&fixed).unwrap().is_empty());
    assert!(
        inspect("bitflags! { struct F: u8 { const A = 1; const B = 2; } }")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn preserves_crlf_and_missing_final_newline() {
    let source = "fn run() {\r\n    let value = r#\"a\r\nb\"#;\r\n    consume(value);\r\n}";
    let fixed = apply(source, &inspect(source).unwrap()).unwrap();

    assert_eq!(
        fixed,
        "fn run() {\r\n    let value = r#\"a\r\nb\"#;\r\n\r\n    consume(value);\r\n}"
    );
    assert!(inspect(&fixed).unwrap().is_empty());
}

#[test]
fn rejects_insertions_that_change_a_string_token() {
    let source = "const TEXT: &str = r#\"one\ntwo\"#;\n";

    let invalid = BTreeMap::from([(
        1,
        Issue {
            rule: "test",
            through_line: 1,
            edit: BlankLineEdit::Insert,
        },
    )]);

    assert!(apply(source, &invalid).is_err());
}

#[test]
fn reports_invalid_rust_without_rewriting_it() {
    assert!(inspect("fn broken( {").is_err());
}

#[test]
fn removes_blank_lines_between_match_arms_and_enum_variants() {
    let source = r#"enum Value {
    First,

    /// The second value.
    Second {
        value: u8,
    },

    #[error("failed")]
    Failed,
}

fn run(value: Value) {
    match value {
        Value::First => {
            first();
        }

        // Process the remaining values together.

        _ => second(),
    }
}
"#;

    let expected = source
        .replace("    First,\n\n", "    First,\n")
        .replace("    },\n\n", "    },\n")
        .replace("        }\n\n", "        }\n")
        .replace("together.\n\n", "together.\n");

    for newline in ["\n", "\r\n"] {
        let source = source.trim_end_matches('\n').replace('\n', newline);
        let expected = expected.trim_end_matches('\n').replace('\n', newline);
        let issues = inspect(&source).unwrap();

        assert_eq!(issues.len(), 4);
        assert_eq!(
            issues
                .values()
                .filter(|issue| issue.rule == "match-arm-blank-lines")
                .count(),
            2
        );
        assert_eq!(
            issues
                .values()
                .filter(|issue| issue.rule == "enum-variant-blank-lines")
                .count(),
            2
        );
        assert_eq!(apply(&source, &issues).unwrap(), expected);
        assert!(inspect(&expected).unwrap().is_empty());
    }
}

#[test]
fn preserves_comment_and_literal_contents_when_removing_blank_lines() {
    let source = r##"enum Value {
    First, /* Keep the nested comment.
    /* Nested detail.

    */

    */

    Second,
}

fn run() {
    match value {
        0 => r#"first

second"#, // This marker is text: /*
<blank>

        1 => "🦀🦀/*", /* Another comment.

        */
<blank>
        _ => "last",
    }
}
"##
    .replace("<blank>", "        ");

    let expected = source
        .replace("    */\n\n    Second", "    */\n    Second")
        .replace("        \n\n", "")
        .replace("        \n", "");

    let issues = inspect(&source).unwrap();

    assert_eq!(issues.len(), 4);
    assert_eq!(apply(&source, &issues).unwrap(), expected);
    assert!(inspect(&expected).unwrap().is_empty());
}

#[test]
fn adds_required_spacing_inside_compacted_groups_and_respects_skip() {
    let source = "enum Value {\n    Named {\n        /// First value.\n        first: u8,\n        /// Second value.\n        second: u8,\n    },\n\n    Empty,\n}\n\nfn run() {\n    match value {\n        0 => {\n            let item = 1;\n            consume(item);\n        }\n\n        _ => {},\n    }\n}\n";

    let expected = source
        .replace("first: u8,\n", "first: u8,\n\n")
        .replace("item = 1;\n", "item = 1;\n\n")
        .replace("    },\n\n", "    },\n")
        .replace("        }\n\n", "        }\n");

    let issues = inspect(source).unwrap();

    assert_eq!(issues.len(), 4);
    assert_eq!(apply(source, &issues).unwrap(), expected);
    assert!(inspect(&expected).unwrap().is_empty());

    let skipped = format!(
        "#[rustfmt::skip]\n{}",
        source.replace("fn run()", "#[rustfmt::skip]\nfn run()")
    );

    assert!(inspect(&skipped).unwrap().is_empty());
}

#[test]
fn separates_visibility_groups_before_attributes_and_comments_with_crlf() {
    let source = "mod outer {\r\n    pub(crate) use crate::api::Public;\r\n    // The helper is shared only inside the parent module.\r\n    #[cfg(test)]\r\n    pub(super) use crate::api::{\r\n        First,\r\n        Second,\r\n    };\r\n    pub(super) use crate::api::Third;\r\n}\r\n";
    let issues = inspect(source).unwrap();
    let fixed = apply(source, &issues).unwrap();

    assert_eq!(issues.len(), 1);
    assert_eq!(issues.values().next().unwrap().rule, "declaration-groups");
    assert_eq!(fixed, source.replace("Public;\r\n", "Public;\r\n\r\n"));
    assert!(inspect(&fixed).unwrap().is_empty());
}

#[test]
fn separates_import_sources_without_detaching_comments_or_attributes() {
    for visibility in ["pub ", "pub(crate) ", "pub(super) ", ""] {
        let source = format!(
            "mod inner {{\r\n    {visibility}use std::fmt;\r\n    {visibility}use core::mem;\r\n    // Serialization types are platform dependent.\r\n    #[cfg(unix)]\r\n    {visibility}use serde::{{\r\n        Serialize,\r\n        Deserialize,\r\n    }};\r\n    {visibility}use crate::api::Public;\r\n}}\r\n"
        );

        let issues = inspect(&source).unwrap();
        let fixed = apply(&source, &issues).unwrap();

        assert_eq!(issues.len(), 2, "{source}");
        assert!(issues.values().all(|issue| issue.rule == "import-groups"));
        assert_eq!(
            fixed,
            source
                .replace("mem;\r\n", "mem;\r\n\r\n")
                .replace("    };\r\n", "    };\r\n\r\n")
        );
        assert!(inspect(&fixed).unwrap().is_empty());
    }
}

#[test]
fn separates_module_documentation_from_code_and_attributes() {
    for (source, expected) in [
        (
            "//! Module details.\n//! More details.\nuse std::fmt;\n",
            "//! Module details.\n//! More details.\n\nuse std::fmt;\n",
        ),
        (
            "//! Module details.\n#![allow(unused)]\nuse std::fmt;\n",
            "//! Module details.\n\n#![allow(unused)]\nuse std::fmt;\n",
        ),
        (
            "mod inner {\r\n    //! Module details.\r\n    /// The entry point.\r\n    fn run() {}\r\n}\r\n",
            "mod inner {\r\n    //! Module details.\r\n\r\n    /// The entry point.\r\n    fn run() {}\r\n}\r\n",
        ),
        (
            "mod inner { //! Module details.\n    use std::fmt;\n}\n",
            "mod inner { //! Module details.\n\n    use std::fmt;\n}\n",
        ),
    ] {
        let issues = inspect(source).unwrap();

        assert_eq!(issues.len(), 1, "{source}");
        assert_eq!(issues.values().next().unwrap().rule, "module-docs");
        assert_eq!(apply(source, &issues).unwrap(), expected);
        assert!(inspect(expected).unwrap().is_empty());
    }

    for source in [
        "//! Documentation only.\n",
        "mod inner {\n    //! Documentation only.\n}\n",
        "/// Function details.\nfn run() {}\n",
        "const TEXT: &str = r#\"//! Literal text.\nuse std::fmt;\"#;\n",
    ] {
        assert!(inspect(source).unwrap().is_empty(), "{source}");
    }
}
