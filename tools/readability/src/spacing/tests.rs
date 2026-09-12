use std::collections::BTreeMap;

use crate::spacing::{self, Issue, Issues, apply};

fn inspect(source: &str) -> Result<Issues, syn::Error> {
    Ok(spacing::inspect(source, &syn::parse_file(source)?))
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
fn separates_documented_fields_and_multiline_match_arms() {
    let source = r#"struct Values {
    /// The first value.
    first: u8,
    /// The second value.
    second: u8,
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

    let issues = inspect(source).unwrap();

    assert_eq!(issues.len(), 2);
    assert!(
        issues
            .values()
            .any(|reasons| reasons.rule == "documented-fields")
    );
    assert!(issues.values().any(|reasons| reasons.rule == "match-arms"));
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
        },
    )]);

    assert!(apply(source, &invalid).is_err());
}

#[test]
fn reports_invalid_rust_without_rewriting_it() {
    assert!(inspect("fn broken( {").is_err());
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
