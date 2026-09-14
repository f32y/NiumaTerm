use crate::declarations::inspect;

#[test]
fn orders_import_sources_within_each_visibility_group() {
    let mut header = String::new();

    for visibility in ["pub ", "pub(crate) ", "pub(super) ", ""] {
        for import in [
            "::core::mem",
            "std::fmt",
            "{alloc::vec::Vec, std::io}",
            "r#serde as serialization",
            "{anyhow::Result, {serde::Serialize}}",
            "crate::api::{self, Public}",
        ] {
            header.push_str(&format!("{visibility}use {import};\n\n"));
        }
    }

    for source in [header.clone(), format!("mod inner {{\n{header}}}\n")] {
        assert!(
            inspect(&syn::parse_file(&source).unwrap().items).is_empty(),
            "{source}"
        );
    }
}

#[test]
fn rejects_reversed_import_sources_even_with_blank_lines() {
    let imports = ["std::fmt", "serde::Serialize", "crate::api::Public"];

    for visibility in ["pub ", "pub(crate) ", "pub(super) ", ""] {
        for (index, earlier) in imports.iter().enumerate() {
            for later in &imports[index + 1..] {
                let source = format!(
                    "{visibility}use {later};\n\n#[cfg(unix)]\n{visibility}use {earlier};\n"
                );

                let issues = inspect(&syn::parse_file(&source).unwrap().items);

                assert_eq!(issues.len(), 1, "{source}");
                assert_eq!(issues[0].rule, "import-order", "{source}");
                assert_eq!(issues[0].span.start().line, 3);
                assert_eq!(issues[0].related.start().line, 1);
            }
        }
    }

    let source = "use crate::api::Public;\n\nuse serde::Serialize;\n\nuse anyhow::Result;\n";
    let issues = inspect(&syn::parse_file(source).unwrap().items);

    assert_eq!(issues.len(), 2);
    assert!(issues.iter().all(|issue| issue.rule == "import-order"));
    assert!(issues.iter().all(|issue| issue.related.start().line == 1));
}

#[test]
fn rejects_mixed_sources_inside_a_single_use_tree() {
    for tree in [
        "{std::fmt, serde::Serialize}",
        "{core::mem, crate::api::Public}",
        "{serde::Serialize, crate::api::Public}",
        "{{alloc::vec::Vec}, {serde as serialization}}",
        "{std::io::*, {crate::api::*}}",
    ] {
        for visibility in ["pub ", "pub(crate) ", "pub(super) ", ""] {
            let source = format!("{visibility}use {tree};\n");
            let issues = inspect(&syn::parse_file(&source).unwrap().items);

            assert_eq!(issues.len(), 1, "{source}");
            assert_eq!(issues[0].rule, "mixed-imports", "{source}");
        }
    }
}

#[test]
fn reports_import_groups_on_one_line_without_duplicate_visibility_issues() {
    for source in [
        "use std::fmt; use serde::Serialize;",
        "use serde::Serialize; use crate::api::Public;",
        "pub use std::fmt; use serde::Serialize;",
    ] {
        let issues = inspect(&syn::parse_file(source).unwrap().items);

        assert_eq!(issues.len(), 1, "{source}");
        assert_eq!(issues[0].rule, "group-spacing", "{source}");
    }
}

#[test]
fn keeps_import_checks_active_in_skipped_modules_but_exempts_local_blocks() {
    let source =
        "#[rustfmt::skip]\nmod inner {\n    use crate::api::Public;\n\n    use std::fmt;\n}\n";

    let issues = inspect(&syn::parse_file(source).unwrap().items);

    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].rule, "import-order");

    let source = "fn run() { use crate::api::Public; use std::fmt; mod local { use {std::io, serde::Serialize}; } }\n\ngenerated! { use {std::fmt, crate::api::Public}; }\n";

    assert!(inspect(&syn::parse_file(source).unwrap().items).is_empty());
}

#[test]
fn checks_alphabetical_order_within_each_visibility_and_source_group() {
    for visibility in ["pub ", "pub(crate) ", "pub(super) ", ""] {
        for (earlier, later) in [
            ("std::fmt", "std::io"),
            ("anyhow::Result", "serde::Serialize"),
            ("crate::alpha::Value", "crate::zeta::Value"),
        ] {
            let ordered = format!("{visibility}use {earlier};\n\n{visibility}use {later};\n");

            let reversed = format!(
                "{visibility}use {later};\n\n// Available on Unix systems.\n#[cfg(unix)]\n{visibility}use {earlier};\n"
            );

            assert!(inspect(&syn::parse_file(&ordered).unwrap().items).is_empty());

            let issues = inspect(&syn::parse_file(&reversed).unwrap().items);

            assert_eq!(issues.len(), 1, "{reversed}");
            assert_eq!(issues[0].rule, "import-alphabetical");
            assert_eq!(issues[0].span.start().line, 4);
            assert_eq!(issues[0].related.start().line, 1);
        }
    }

    let source = "pub use crate::zeta::Value;\n\npub(crate) use crate::alpha::Value;\n\nuse std::fmt;\n\nuse anyhow::Result;\n\nuse crate::alpha::Value;\n";

    assert!(inspect(&syn::parse_file(source).unwrap().items).is_empty());
}

#[test]
fn checks_nested_import_lists_and_keeps_the_earlier_out_of_order_path() {
    let source =
        "use crate::api::{\n    Zebra,\n    Alpha,\n    Beta,\n    nested::{Zulu, Alpha},\n};\n";

    let issues = inspect(&syn::parse_file(source).unwrap().items);

    assert_eq!(issues.len(), 3);
    assert!(
        issues
            .iter()
            .all(|issue| issue.rule == "import-alphabetical")
    );
    assert_eq!(issues[0].span.start().line, 3);
    assert_eq!(issues[0].related.start().line, 2);
    assert_eq!(issues[1].span.start().line, 4);
    assert_eq!(issues[1].related.start().line, 2);
    assert_eq!(issues[2].span.start().line, 5);

    let source = "use crate::zeta::Value;\nuse crate::alpha::Value;\nuse crate::beta::Value;\n";
    let issues = inspect(&syn::parse_file(source).unwrap().items);

    assert_eq!(issues.len(), 2);
    assert!(issues.iter().all(|issue| issue.related.start().line == 1));
}

#[test]
fn accepts_rust_2024_name_order_aliases_and_equivalent_path_spellings() {
    for source in [
        "use crate::api::{self, _alpha, Alpha, Zebra, a_, a_a, a1, a002, a02, a2, a10, aA, aa, z, ä};\n",
        "use crate::api::{a1a, a01z, a02a, a2a, a002b};\n",
        "use crate::api::{mod99999999999999999999, mod100000000000000000000};\n",
        "use crate::alpha as zeta;\nuse crate::zeta as alpha;\n",
        "use crate::api::{Foo as zed, Foo as alpha, Foo};\n",
        "use ::r#alpha::A;\nuse alpha::Z;\nuse r#zebra::A;\n",
        "use std;\nuse std::error;\nuse std::ffi::CStr;\nuse std::io as io_alias;\nuse std::io::Error;\nuse std::io::*;\nuse std::io::{self, Read};\nuse std::{error, io};\n",
        "use crate::api::{foo, foo as alias, foo::A, foo::B, foo::*, foo::{A, B}};\n",
        "use crate::foo::{self};\nuse crate::foo;\nuse crate::foo::{self as renamed};\nuse crate::foo::Bar;\n",
    ] {
        assert!(
            inspect(&syn::parse_file(source).unwrap().items).is_empty(),
            "{source}"
        );
    }

    for source in [
        "use crate::api::{Zebra, _alpha};\n",
        "use crate::api::{alpha, Alpha};\n",
        "use crate::api::{mod10, mod2};\n",
        "use crate::api::{mod2, mod02};\n",
        "use crate::api::{zeta as alpha, alpha as zeta};\n",
        "use r#zebra::A;\nuse alpha::Z;\n",
    ] {
        let issues = inspect(&syn::parse_file(source).unwrap().items);

        assert_eq!(issues.len(), 1, "{source}");
        assert_eq!(issues[0].rule, "import-alphabetical", "{source}");
    }
}

#[test]
fn checks_alphabetical_order_in_nested_and_skipped_modules_only() {
    for attribute in ["", "#[rustfmt::skip]\n"] {
        let source = format!("{attribute}mod inner {{\n    use std::io;\n    use std::fmt;\n}}\n");
        let issues = inspect(&syn::parse_file(&source).unwrap().items);

        assert_eq!(issues.len(), 1, "{source}");
        assert_eq!(issues[0].rule, "import-alphabetical");
    }

    let source = "fn run() { use std::io; use std::fmt; mod local { use crate::{Zebra, Alpha}; } }\n\ngenerated! { use crate::{Zebra, Alpha}; }\n";

    assert!(inspect(&syn::parse_file(source).unwrap().items).is_empty());
}
