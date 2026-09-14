use crate::declarations::inspect;

const GROUPS: [&str; 9] = [
    "pub use crate::api::Public;",
    "pub(crate) use crate::api::Internal;",
    "pub(super) use crate::api::Parent;",
    "pub mod api;",
    "pub(crate) mod shared;",
    "pub(super) mod sibling;",
    "mod private;",
    "#[cfg(test)]\nmod tests;",
    "use std::fmt;",
];

#[test]
fn accepts_ordered_headers_with_attributes_and_nested_modules() {
    let source = format!(
        "//! Library entry points.\n#![allow(unused)]\n\n{}\n\nfn run() {{}}\n",
        GROUPS.join("\n\n")
    );

    assert!(inspect(&syn::parse_file(&source).unwrap().items).is_empty());

    let source = "#[cfg(test)]\nmod tests {\n    pub use crate::api::{\n        First,\n        Second,\n    };\n\n    pub(crate) use crate::api::Internal;\n\n    pub(super) use crate::api::Parent;\n\n    pub mod public;\n\n    pub(crate) mod shared;\n\n    pub(super) mod sibling;\n\n    mod private;\n\n    use std::fmt;\n\n    fn run() {}\n}\n";

    assert!(inspect(&syn::parse_file(source).unwrap().items).is_empty());
}

#[test]
fn rejects_every_reversed_group_pair() {
    for (index, earlier) in GROUPS.iter().enumerate() {
        for later in &GROUPS[index + 1..] {
            let source = format!("{later}\n\n{earlier}\n");
            let issues = inspect(&syn::parse_file(&source).unwrap().items);

            assert_eq!(issues.len(), 1, "{source}");
            assert_eq!(issues[0].rule, "order", "{source}");
            assert_eq!(issues[0].span.start().line, later.lines().count() + 2);
            assert_eq!(issues[0].related.start().line, 1);
        }
    }
}

#[test]
fn rejects_declarations_after_other_items_in_each_module() {
    for body in [
        "fn run() {}",
        "const N: u8 = 0;",
        "static N: u8 = 0;",
        "struct Value;",
        "enum Value {}",
        "type Value = u8;",
        "impl Value {}",
        "trait Value {}",
        "extern crate demo;",
        "initialize!();",
        "macro_rules! generated { () => {} }",
    ] {
        for declaration in GROUPS {
            let source = format!("{body}\n\n{declaration}\n");
            let issues = inspect(&syn::parse_file(&source).unwrap().items);

            assert_eq!(issues.len(), 1, "{source}");
            assert_eq!(issues[0].rule, "header", "{source}");
        }
    }

    let source = "mod outer { mod inner { fn run() {} use std::fmt; } }";
    let issues = inspect(&syn::parse_file(source).unwrap().items);

    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].rule, "header");
}

#[test]
fn checks_visibility_on_all_declaration_kinds_and_in_nested_bodies() {
    for declaration in [
        "$vis mod inner;",
        "$vis mod tests {}",
        "$vis use std::fmt;",
        "$vis extern crate demo;",
        "$vis fn run() {}",
        "$vis struct Value;",
        "$vis enum Value { Empty }",
        "$vis union Value { byte: u8 }",
        "$vis type Value = u8;",
        "$vis trait Run {}",
        "$vis trait Run = Send;",
        "$vis const N: u8 = 0;",
        "$vis static N: u8 = 0;",
        "struct Value { $vis field: u8 }",
        "struct Value($vis u8);",
        "union Value { $vis field: u8 }",
        "impl Value { $vis fn run(&self) {} }",
        "impl Value { $vis const N: u8 = 0; }",
        "impl Value { $vis type Output = u8; }",
        "unsafe extern \"C\" { $vis fn run(); }",
        "unsafe extern \"C\" { $vis static N: u8; }",
        "unsafe extern \"C\" { $vis type Opaque; }",
        "mod outer { $vis fn run() {} }",
        "fn run() { $vis use std::fmt; }",
        "fn run() { $vis mod local {} }",
        "fn run() { $vis struct Local; }",
        "impl Value { fn run() { $vis fn local() {} } }",
        "trait Run { fn run() { $vis type Local = u8; } }",
        "const CALLBACK: fn() = || { $vis struct Local; };",
        "#[rustfmt::skip] fn run() { $vis struct Local; }",
        "#[cfg(any())] $vis struct Disabled;",
    ] {
        for (visibility, rejected) in [
            ("pub(self)", true),
            ("pub(in crate)", true),
            ("pub(in crate::outer)", true),
            ("", false),
            ("pub(super)", false),
            ("pub(crate)", false),
            ("pub", false),
        ] {
            let source = declaration.replace("$vis", visibility);
            let issues = inspect(&syn::parse_file(&source).unwrap().items);

            assert_eq!(issues.len(), usize::from(rejected), "{source}");
            assert!(
                issues.iter().all(|issue| issue.rule == "visibility"),
                "{source}"
            );
        }
    }
}

#[test]
fn exempts_local_declaration_order_and_does_not_parse_macro_input() {
    let source = "fn run() { action(); use std::fmt; mod local_tests { fn a() {} use std::io; } }\n\nimpl Value { fn run() { use std::fmt; mod local_tests {} } }\n\ntrait Run { fn run() { action(); use std::fmt; } }\n\nconst CALLBACK: fn() = || { action(); use std::fmt; };\n\ngenerated! { fn run() {} mod tests; use std::fmt; }\n";

    assert!(inspect(&syn::parse_file(source).unwrap().items).is_empty());

    let source = "// pub(self) struct Comment;\nconst TEXT: &str = \"pub(in crate) fn text() {}\";\n\nmacro_rules! generate { () => { pub(self) struct Generated; } }\n\ngenerate! { pub(in crate) fn run() {} }\n";

    assert!(inspect(&syn::parse_file(source).unwrap().items).is_empty());
}

#[test]
fn requires_explicit_test_cfg_on_every_matching_module_name() {
    for name in [
        "tests",
        "parser_tests",
        "test_support_extra",
        "contest",
        "r#test",
    ] {
        for attribute in [
            "",
            "#[cfg(windows)]",
            "#[cfg(not(test))]",
            "#[cfg(all(test, windows))]",
            "#[cfg(any(test, windows))]",
            "#[cfg_attr(test, cfg(test))]",
            "#[test]",
        ] {
            let source = format!("{attribute}\nmod {name};\n");
            let issues = inspect(&syn::parse_file(&source).unwrap().items);

            assert_eq!(issues.len(), 1, "{source}");
            assert_eq!(issues[0].rule, "test-module-cfg", "{source}");
        }
    }

    let source = "#[cfg(test)]\nmod tests {\n    mod nested_tests;\n}\n";
    let issues = inspect(&syn::parse_file(source).unwrap().items);

    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].rule, "test-module-cfg");
    assert_eq!(issues[0].span.start().line, 3);
}

#[test]
fn groups_test_modules_after_normal_modules_regardless_of_visibility() {
    let source = "mod runtime;\n\n#[cfg(test)]\nmod tests;\n#[cfg(test)]\npub(super) mod parser_tests;\n#[cfg(test)]\npub(crate) mod test_support;\n#[cfg(test)]\npub mod integration_tests;\n\nuse std::fmt;\n";

    assert!(inspect(&syn::parse_file(source).unwrap().items).is_empty());
}

#[test]
fn exempts_only_test_support_from_the_test_attribute_requirement() {
    for name in ["test_support", "r#test_support"] {
        let source = format!(
            "mod runtime;\n\n#[cfg(any(test, feature = \"test-support\"))]\npub mod {name};\n\nuse std::fmt;\n"
        );

        assert!(inspect(&syn::parse_file(&source).unwrap().items).is_empty());

        let source = format!("mod {name};\n\nmod runtime;\n");
        let issues = inspect(&syn::parse_file(&source).unwrap().items);

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].rule, "order");
    }

    let issues = inspect(&syn::parse_file("mod shared_test_support;").unwrap().items);

    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].rule, "test-module-cfg");
}

#[test]
fn allows_inline_test_modules_after_code_but_checks_their_contents() {
    for name in ["tests", "parser_tests", "r#test"] {
        let source = format!(
            "mod runtime;\n\nuse std::fmt;\n\nfn run() {{}}\n\nmod {name} {{\n    use std::io;\n\n    fn check() {{}}\n}}\n"
        );

        assert!(inspect(&syn::parse_file(&source).unwrap().items).is_empty());
    }

    let source =
        "fn run() {}\n\n#[cfg(test)]\nmod tests {\n    fn check() {}\n\n    use std::fmt;\n}\n";

    let issues = inspect(&syn::parse_file(source).unwrap().items);

    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].rule, "header");
    assert_eq!(issues[0].span.start().line, 7);

    let source = "mod tests {}\n\nuse std::fmt;\n";
    let issues = inspect(&syn::parse_file(source).unwrap().items);

    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].rule, "header");
    assert_eq!(issues[0].span.start().line, 3);
}

#[test]
fn allows_private_inline_modules_among_body_items_after_imports() {
    let source = "mod external;\n\nuse std::fmt;\n\nmod first {\n    use std::io;\n\n    fn run() {}\n\n    mod nested {}\n}\n\nfn run() {}\n\n#[allow(dead_code)]\nmod second {}\n\nconst N: u8 = 0;\n";

    assert!(inspect(&syn::parse_file(source).unwrap().items).is_empty());

    for visibility in ["pub ", "pub(crate) ", "pub(super) "] {
        let source = format!("fn run() {{}}\n\n{visibility}mod inner {{}}\n");
        let issues = inspect(&syn::parse_file(&source).unwrap().items);

        assert_eq!(issues.len(), 1, "{source}");
        assert_eq!(issues[0].rule, "header", "{source}");
    }
}

#[test]
fn rejects_imports_after_private_inline_modules_and_checks_nested_headers() {
    for source in [
        "mod sys {}\n\nuse std::fmt;\n",
        "#[allow(dead_code)]\nmod sys {\n    fn run() {}\n}\n\nuse std::fmt;\n",
        "mod outer {\n    mod sys {}\n\n    use std::fmt;\n}\n",
        "use std::fmt;\n\nmod sys {}\n\nuse serde::Serialize;\n",
    ] {
        let issues = inspect(&syn::parse_file(source).unwrap().items);

        assert_eq!(issues.len(), 1, "{source}");
        assert_eq!(issues[0].rule, "header", "{source}");
    }
}

#[test]
fn rejects_module_paths_in_attributes_and_conditional_attributes() {
    for source in [
        "#[path = \"other.rs\"]\nmod inner;\n",
        "#[cfg_attr(unix, path = \"other.rs\")]\nmod inner;\n",
        "#[cfg_attr(unix, cfg_attr(feature = \"extra\", path = \"other.rs\"))]\nmod inner;\n",
        "mod outer {\n    #[path = \"other.rs\"]\n    mod inner;\n}\n",
        "fn run() {\n    #[path = \"other.rs\"]\n    mod local;\n}\n",
        "#[rustfmt::skip]\nmod tests {\n    #[path = \"other.rs\"]\n    mod inner;\n}\n",
        "#[cfg(all(test, unix))]\n#[path = \"other.rs\"]\nmod inner;\n",
        "#[cfg_attr(test, cfg(test))]\n#[path = \"other.rs\"]\nmod inner;\n",
        "#[cfg(test)]\nmod outer {\n    #[path = \"other.rs\"]\n    mod inner;\n}\n",
        "#[cfg(test)]\n#[path = \"other.rs\"]\nfn run() {}\n",
    ] {
        let issues = inspect(&syn::parse_file(source).unwrap().items);

        assert_eq!(issues.len(), 1, "{source}");
        assert_eq!(issues[0].rule, "path-attribute", "{source}");
    }

    let source = "#[cfg_attr(path = \"setting\", allow(unused))]\nmod inner;\n\nconst TEXT: &str = r#\"#[path = \"other.rs\"]\"#;\n\nmake_modules! { #[path = \"other.rs\"] mod generated; }\n";

    assert!(inspect(&syn::parse_file(source).unwrap().items).is_empty());
}

#[test]
fn permits_paths_only_on_modules_with_their_own_explicit_test_cfg() {
    for source in [
        "#[cfg(test)]\n#[path = \"other.rs\"]\nmod tests;\n",
        "#[path = \"other.rs\"]\n#[cfg(test)]\npub(super) mod fixtures;\n",
        "#[cfg(test)]\n#[cfg_attr(unix, path = \"other.rs\")]\nmod fixtures;\n",
        "#[cfg(test)]\n#[cfg_attr(unix, cfg_attr(feature = \"extra\", path = \"other.rs\"))]\nmod fixtures;\n",
        "fn run() {\n    #[cfg(test)]\n    #[path = \"other.rs\"]\n    mod fixtures;\n}\n",
        "#[cfg(test)]\n#[path = \"outer\"]\nmod fixtures {\n    #[cfg(test)]\n    #[path = \"inner.rs\"]\n    mod inner;\n}\n",
    ] {
        assert!(
            inspect(&syn::parse_file(source).unwrap().items).is_empty(),
            "{source}"
        );
    }

    let source = "#[cfg(test)]\n#[path = \"outer\"]\npub(self) mod fixtures {\n    pub(in crate) fn run() {}\n}\n";
    let issues = inspect(&syn::parse_file(source).unwrap().items);

    assert_eq!(issues.len(), 2);
    assert!(issues.iter().all(|issue| issue.rule == "visibility"));

    let source = "#[cfg(test)]\n#[path = \"outer\"]\npub(crate) mod fixtures {\n    #[path = \"inner.rs\"]\n    mod inner;\n}\n\n#[path = \"sibling.rs\"]\nmod sibling;\n";
    let issues = inspect(&syn::parse_file(source).unwrap().items);

    assert_eq!(issues.len(), 2);
    assert!(issues.iter().all(|issue| issue.rule == "path-attribute"));
}
