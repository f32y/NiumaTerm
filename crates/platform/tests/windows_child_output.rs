#![cfg(windows)]
//! Runs a real `cmd.exe` so its own diagnostics, which it writes in the
//! console OEM code page rather than UTF-8, go through the child output
//! decoder the way production captures do.

use nmt_platform::process::{decode_child_output, hidden_cmd_command};

/// `cmd.exe` echoes an unknown command name back inside its error line. On a
/// system whose OEM code page is a multibyte one, the echoed non-ASCII name
/// arrives as that code page's bytes; the decoder has to bring it back
/// verbatim instead of replacement characters.
#[test]
fn cmd_diagnostic_round_trips_non_ascii_text() {
    let name = "\u{4e0d}\u{5b58}\u{5728}\u{7684}\u{547d}\u{4ee4}";
    let output = hidden_cmd_command(name)
        .output()
        .expect("cmd.exe should start");

    let text = decode_child_output(&output.stderr);
    assert!(
        text.contains(name),
        "decoded stderr should contain the echoed name, got {text:?}"
    );
    assert!(
        !text.contains('\u{fffd}'),
        "no replacement characters in {text:?}"
    );
}

#[test]
fn utf8_input_is_returned_unchanged() {
    let text = "caf\u{e9} \u{4e2d}\u{6587}";
    assert_eq!(decode_child_output(text.as_bytes()), text);
}
