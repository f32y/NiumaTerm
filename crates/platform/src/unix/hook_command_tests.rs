use crate::unix::hook_command::{build_hook_command, hook_command_contains};

#[test]
fn plain_path_stays_unquoted() {
    let command = build_hook_command("/opt/NiumaTerm/nmt-agent-hook", "notify").unwrap();

    assert_eq!(command, "/opt/NiumaTerm/nmt-agent-hook notify");
    assert!(hook_command_contains(&command, "nmt-agent-hook"));
}

#[test]
fn path_with_spaces_is_quoted() {
    let command = build_hook_command("/Applications/Niuma Term/hook", "notify").unwrap();

    assert_eq!(command, "'/Applications/Niuma Term/hook' notify");
}

#[test]
fn embedded_quote_closes_and_reopens_the_literal() {
    let command = build_hook_command("/home/o'brien/hook", "notify").unwrap();

    assert_eq!(command, r"'/home/o'\''brien/hook' notify");
}

#[test]
fn leading_tilde_is_quoted_against_expansion() {
    let command = build_hook_command("~/bin/hook", "notify").unwrap();

    assert_eq!(command, "'~/bin/hook' notify");
}

#[test]
fn unsafe_argument_is_rejected() {
    assert!(build_hook_command("/opt/hook", "notify; rm -rf /").is_err());
    assert!(build_hook_command("", "notify").is_err());
}
