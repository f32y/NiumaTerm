use crate::expressions::{self, Issue, Options};

fn inspect(file: &syn::File) -> Vec<Issue> {
    expressions::inspect(
        file,
        &Options {
            fixed_option_returns: true,
        },
    )
}

#[test]
fn default_options_disable_only_fixed_option_returns() {
    let source = "fn value() -> Option<u8> { Some((|| 1)()) }";
    let issues = expressions::inspect(&syn::parse_file(source).unwrap(), &Options::default());

    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].rule, "immediate-closure-call");
}

#[test]
fn rejects_immediate_calls_to_closure_expressions() {
    for expression in [
        "(|| 1)()",
        "(move || value)()",
        "(|value: u8| value + 1)(1)",
        "(((|| 1)))()",
        "(|| -> Result<(), String> { write()?; Ok(()) })()?",
        "(async || 1)().await",
    ] {
        let source = format!("async fn run() {{\n    let result = {expression};\n}}\n");
        let issues = inspect(&syn::parse_file(&source).unwrap());

        assert_eq!(issues.len(), 1, "{source}");
        assert_eq!(issues[0].rule, "immediate-closure-call");
        assert_eq!(issues[0].span.start().line, 2);
    }
}

#[test]
fn checks_nested_closure_calls_and_ignores_spacing_exemptions() {
    let source = "#[rustfmt::skip]\nmod inner {\n    fn run() {\n        let result = (|| {\n            (|| 1)()\n        })();\n    }\n}\n";
    let issues = inspect(&syn::parse_file(source).unwrap());

    assert_eq!(issues.len(), 2);
    assert_eq!(issues[0].span.start().line, 4);
    assert_eq!(issues[0].span.end().line, 6);
    assert_eq!(issues[1].span.start().line, 5);
}

#[test]
fn allows_callbacks_stored_closures_and_ordinary_function_calls() {
    for expression in [
        "values.map(|value| value + 1)",
        "invoke(|| 1)",
        "{ let callback = || 1; callback() }",
        "named_function()",
        "(named_function)()",
        "make_callback()()",
        "async { work().await }.await",
        "stringify!((|| 1)())",
        "\"(|| 1)()\"",
    ] {
        let source = format!("fn run() {{ let result = {expression}; }}");

        assert!(
            inspect(&syn::parse_file(&source).unwrap()).is_empty(),
            "{source}"
        );
    }
}

#[test]
fn rejects_direct_fixed_option_returns() {
    for body in [
        "Some(1)",
        "Some(compute())",
        "None",
        "return Some(1);",
        "return None;",
        "((Some(1)))",
        "Option::Some(1)",
        "Option::<u8>::None",
        "std::option::Option::Some(1)",
        "core::option::Option::None",
        "if ready { return Some(1); } Some(2)",
        "if ready { return None; } None",
        "lookup()?; None",
    ] {
        let source = format!("fn value() -> Option<u8> {{ {body} }}");
        let issues = inspect(&syn::parse_file(&source).unwrap());

        assert_eq!(issues.len(), 1, "{source}");
        assert_eq!(issues[0].rule, "fixed-option-return");
    }
}

#[test]
fn preserves_option_returns_with_propagation_or_unknown_variants() {
    for body in [
        "Some(home_dir()?.join(\".claude\").join(\"projects\"))",
        "let value = lookup()?; Some(value)",
        "if ready { return None; } Some(1)",
        "if ready { return Some(1); } None",
        "if ready { return lookup(); } Some(1)",
        "Some({ if ready { return None; } 1 })",
        "let inner: Option<u8> = try { if ready { return None; } 1 }; Some(1)",
        "lookup()",
        "lookup().map(|value| value + 1)",
        "if ready { Some(1) } else { None }",
        "may_return!(); Some(1)",
        "Some(generate_value!())",
    ] {
        let source = format!("fn value() -> Option<u8> {{ {body} }}");

        assert!(
            inspect(&syn::parse_file(&source).unwrap()).is_empty(),
            "{source}"
        );
    }
}

#[test]
fn distinguishes_nested_return_scopes_from_the_enclosing_function() {
    for body in [
        "let callback = || { return None::<u8>; }; Some(1)",
        "let callback = || -> Option<u8> { lookup()?; None }; Some(1)",
        "let future = async { lookup()?; None::<u8> }; Some(1)",
        "fn nested() -> Option<u8> { lookup() } Some(1)",
        "let inner: Option<u8> = try { lookup()? }; Some(1)",
    ] {
        let source = format!("fn value() -> Option<u8> {{ {body} }}");
        let issues = inspect(&syn::parse_file(&source).unwrap());

        assert_eq!(issues.len(), 1, "{source}");
        assert_eq!(issues[0].rule, "fixed-option-return");
        assert!(issues[0].message.contains("only returns Some"));
    }
}

#[test]
fn checks_option_return_types_on_functions_and_methods() {
    for source in [
        "async fn value() -> std::option::Option<u8> { Some(1) }",
        "fn value() -> (core::option::Option<u8>) { None }",
        "impl Value { fn value(&self) -> Option<u8> { Some(1) } }",
        "trait Value { fn value(&self) -> Option<u8> { None } }",
        "fn outer() { fn nested() -> Option<u8> { None } }",
        "#[rustfmt::skip] fn value() -> Option<u8> { None }",
    ] {
        let issues = inspect(&syn::parse_file(source).unwrap());

        assert_eq!(issues.len(), 1, "{source}");
        assert_eq!(issues[0].rule, "fixed-option-return");
    }

    for source in [
        "fn value() -> custom::Option<u8> { Some(1) }",
        "fn value() -> Maybe<u8> { None }",
        "fn value() -> Result<u8, Error> { Ok(1) }",
        "trait Value { fn value(&self) -> Option<u8>; }",
    ] {
        assert!(
            inspect(&syn::parse_file(source).unwrap()).is_empty(),
            "{source}"
        );
    }
}
