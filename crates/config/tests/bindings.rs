use nmt_config::bindings::Bindings;
use serde::{Deserialize, Serialize};
use toml::from_str;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Root {
    #[serde(default = "Bindings::default")]
    bindings: Bindings,
}

#[test]
fn test_valid_key_action() {
    let content = r#"
        [bindings]
        keys = [
            { key = 'Q', with = 'super', action = 'quit' }
        ]
    "#;

    let decoded = from_str::<Root>(content).unwrap();
    assert_eq!(decoded.bindings.keys[0].key, "Q");
    assert_eq!(decoded.bindings.keys[0].with.to_owned(), "super");
    assert_eq!(decoded.bindings.keys[0].action.to_owned(), "quit");
    assert!(decoded.bindings.keys[0].esc.to_owned().is_empty());
}

#[test]
fn test_mode_key_input() {
    let content = r"
        [bindings]
        keys = [
            { key = 'Home', esc = '\x1bOH', mode = 'appcursor' },
        ]
    ";

    let decoded = from_str::<Root>(content).unwrap();
    assert_eq!(decoded.bindings.keys[0].key, "Home");
    assert_eq!(decoded.bindings.keys[0].with, "");
    assert_eq!(decoded.bindings.keys[0].mode, "appcursor");
    assert_eq!(decoded.bindings.keys[0].action.to_owned(), "");
    assert!(!decoded.bindings.keys[0].esc.to_owned().is_empty());
}

#[test]
fn test_escape_sequences() {
    // Test with Unicode escape sequences in double quotes (TOML standard)
    let content = r#"
        [bindings]
        keys = [
            { key = 'l', with = 'control', esc = "\u001b[2J\u001b[H" },
        ]
    "#;

    let decoded = from_str::<Root>(content).unwrap();
    assert_eq!(decoded.bindings.keys[0].esc, "\x1b[2J\x1b[H");
    assert_eq!(decoded.bindings.keys[0].esc.as_bytes(), b"\x1b[2J\x1b[H");
}
