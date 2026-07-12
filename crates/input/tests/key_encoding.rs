//! Characterization (golden) tests for the encoder. The function had **zero** tests
//! before it was moved here; these lock its current byte output so the rio-input
//! extraction and later refactors are verifiably non-regressing.

use nmt_input::event::ElementState;
use nmt_input::keyboard::{Key, KeyLocation, ModifiersState, NamedKey};
use nmt_input::{
    KeyEncodeFlags, KeyInput, KeyUpAction, bracket_paste, build_key_sequence, encode_mouse_report,
    encode_terminal_input, encode_terminal_key, key_up_action,
};

fn named(key: NamedKey) -> KeyInput {
    KeyInput {
        logical_key: Key::Named(key),
        key_without_modifiers: Key::Named(key),
        text_with_all_modifiers: None,
        location: KeyLocation::Standard,
        state: ElementState::Pressed,
        repeat: false,
    }
}

fn character(c: &str) -> KeyInput {
    let k = Key::Character(c.into());
    KeyInput {
        logical_key: k.clone(),
        key_without_modifiers: k,
        text_with_all_modifiers: None,
        location: KeyLocation::Standard,
        state: ElementState::Pressed,
        repeat: false,
    }
}

fn seq(input: &KeyInput, mods: ModifiersState, flags: KeyEncodeFlags) -> Vec<u8> {
    build_key_sequence(input, mods, flags)
}

// --- Legacy (non-kitty) named keys: the encoder emits CSI forms. ---

#[test]
fn legacy_arrow_up_no_mods() {
    let got = seq(
        &named(NamedKey::ArrowUp),
        ModifiersState::empty(),
        KeyEncodeFlags::empty(),
    );
    assert_eq!(got, b"\x1b[A");
}

#[test]
fn legacy_arrow_up_ctrl() {
    let got = seq(
        &named(NamedKey::ArrowUp),
        ModifiersState::CONTROL,
        KeyEncodeFlags::empty(),
    );
    assert_eq!(got, b"\x1b[1;5A");
}

#[test]
fn legacy_f5_no_mods() {
    let got = seq(
        &named(NamedKey::F5),
        ModifiersState::empty(),
        KeyEncodeFlags::empty(),
    );
    assert_eq!(got, b"\x1b[15~");
}

#[test]
fn legacy_delete_no_mods() {
    let got = seq(
        &named(NamedKey::Delete),
        ModifiersState::empty(),
        KeyEncodeFlags::empty(),
    );
    assert_eq!(got, b"\x1b[3~");
}

#[test]
fn legacy_delete_ctrl() {
    let got = seq(
        &named(NamedKey::Delete),
        ModifiersState::CONTROL,
        KeyEncodeFlags::empty(),
    );
    assert_eq!(got, b"\x1b[3;5~");
}

/// Pins `build_key_sequence` alone to the F1 CSI form `\x1b[P`, not the
/// SS3 `\x1bOP` that rioterm's binding table currently emits. The app-cursor/SS3
/// distinction lives in the binding rows; this test pins the encoder's current boundary.
#[test]
fn legacy_f1_is_csi_not_ss3() {
    let got = seq(
        &named(NamedKey::F1),
        ModifiersState::empty(),
        KeyEncodeFlags::empty(),
    );
    assert_eq!(got, b"\x1b[P");
}

/// Plain text keys produce nothing in legacy mode — they go through the frontend's
/// text path (WM_CHAR / KeyEvent.text), not the encoder.
#[test]
fn legacy_character_is_empty() {
    let got = seq(
        &character("a"),
        ModifiersState::empty(),
        KeyEncodeFlags::empty(),
    );
    assert!(got.is_empty());
}

// --- Kitty protocol. ---

#[test]
fn kitty_character_disambiguate() {
    let got = seq(
        &character("a"),
        ModifiersState::empty(),
        KeyEncodeFlags::DISAMBIGUATE_ESC_CODES,
    );
    assert_eq!(got, b"\x1b[97u");
}

#[test]
fn kitty_escape_disambiguate() {
    let got = seq(
        &named(NamedKey::Escape),
        ModifiersState::empty(),
        KeyEncodeFlags::DISAMBIGUATE_ESC_CODES,
    );
    assert_eq!(got, b"\x1b[27u");
}

fn released(key: NamedKey) -> KeyInput {
    let mut k = named(key);
    k.state = ElementState::Released;
    k
}

// --- Kitty key-release reporting (REPORT_EVENT_TYPES). ---

#[test]
fn kitty_release_reports_event_type_3() {
    // With REPORT_EVENT_TYPES a key-up encodes a kitty release event (`:3`).
    let got = seq(
        &released(NamedKey::Escape),
        ModifiersState::empty(),
        KeyEncodeFlags::DISAMBIGUATE_ESC_CODES | KeyEncodeFlags::REPORT_EVENT_TYPES,
    );
    assert_eq!(got, b"\x1b[27;1:3u");
}

#[test]
fn kitty_press_omits_event_type() {
    // A plain press carries no event type (`:1` press is the implicit default, only
    // repeat `:2` / release `:3` are emitted), so press stays `\x1b[27u` and is still
    // distinguishable from the `:3` release above.
    let press = seq(
        &named(NamedKey::Escape),
        ModifiersState::empty(),
        KeyEncodeFlags::DISAMBIGUATE_ESC_CODES | KeyEncodeFlags::REPORT_EVENT_TYPES,
    );
    assert_eq!(press, b"\x1b[27u");
    assert_ne!(press, b"\x1b[27;1:3u");
}

#[test]
fn release_without_event_types_is_a_phantom_press() {
    // WHY the NiumaTerm WndProc gates key-up encoding on REPORT_EVENT_TYPES: without it,
    // a Released key encodes the SAME bytes as a press — a phantom press. So a
    // key-up must not be encoded unless event-type reporting is active.
    let release = seq(
        &released(NamedKey::Escape),
        ModifiersState::empty(),
        KeyEncodeFlags::DISAMBIGUATE_ESC_CODES,
    );
    let press = seq(
        &named(NamedKey::Escape),
        ModifiersState::empty(),
        KeyEncodeFlags::DISAMBIGUATE_ESC_CODES,
    );
    assert_eq!(release, press);
    assert_eq!(release, b"\x1b[27u");
}

// --- encode_terminal_key: the unified entry point (migrated binding-row behavior). ---

fn enc(input: &KeyInput, mods: ModifiersState, flags: KeyEncodeFlags) -> Option<Vec<u8>> {
    encode_terminal_key(input, mods, flags)
}

#[test]
fn enc_app_cursor_arrow_is_ss3() {
    let got = enc(
        &named(NamedKey::ArrowUp),
        ModifiersState::empty(),
        KeyEncodeFlags::APP_CURSOR,
    );
    assert_eq!(got.as_deref(), Some(&b"\x1bOA"[..]));
}

#[test]
fn enc_app_cursor_home_is_ss3() {
    let got = enc(
        &named(NamedKey::Home),
        ModifiersState::empty(),
        KeyEncodeFlags::APP_CURSOR,
    );
    assert_eq!(got.as_deref(), Some(&b"\x1bOH"[..]));
}

#[test]
fn enc_non_app_cursor_arrow_is_csi() {
    let got = enc(
        &named(NamedKey::ArrowUp),
        ModifiersState::empty(),
        KeyEncodeFlags::empty(),
    );
    assert_eq!(got.as_deref(), Some(&b"\x1b[A"[..]));
}

/// App-cursor only overrides the unmodified arrow; with a modifier it falls to the
/// generic CSI form (the binding row matched mods == empty exactly).
#[test]
fn enc_app_cursor_arrow_with_ctrl_is_csi() {
    let got = enc(
        &named(NamedKey::ArrowUp),
        ModifiersState::CONTROL,
        KeyEncodeFlags::APP_CURSOR,
    );
    assert_eq!(got.as_deref(), Some(&b"\x1b[1;5A"[..]));
}

#[test]
fn enc_f1_legacy_is_ss3() {
    let got = enc(
        &named(NamedKey::F1),
        ModifiersState::empty(),
        KeyEncodeFlags::empty(),
    );
    assert_eq!(got.as_deref(), Some(&b"\x1bOP"[..]));
}

/// In kitty (disambiguate) mode F1 is no longer overridden to SS3.
#[test]
fn enc_f1_disambiguate_is_csi() {
    let got = enc(
        &named(NamedKey::F1),
        ModifiersState::empty(),
        KeyEncodeFlags::DISAMBIGUATE_ESC_CODES,
    );
    assert_eq!(got.as_deref(), Some(&b"\x1b[P"[..]));
}

#[test]
fn enc_delete_is_csi_tilde() {
    let got = enc(
        &named(NamedKey::Delete),
        ModifiersState::empty(),
        KeyEncodeFlags::empty(),
    );
    assert_eq!(got.as_deref(), Some(&b"\x1b[3~"[..]));
}

#[test]
fn enc_backspace_is_del() {
    let got = enc(
        &named(NamedKey::Backspace),
        ModifiersState::empty(),
        KeyEncodeFlags::empty(),
    );
    assert_eq!(got.as_deref(), Some(&b"\x7f"[..]));
}

#[test]
fn enc_alt_backspace_is_esc_del() {
    let got = enc(
        &named(NamedKey::Backspace),
        ModifiersState::ALT,
        KeyEncodeFlags::empty(),
    );
    assert_eq!(got.as_deref(), Some(&b"\x1b\x7f"[..]));
}

#[test]
fn enc_shift_tab_is_csi_z() {
    let got = enc(
        &named(NamedKey::Tab),
        ModifiersState::SHIFT,
        KeyEncodeFlags::empty(),
    );
    assert_eq!(got.as_deref(), Some(&b"\x1b[Z"[..]));
}

/// Plain text keys return None — the caller delivers them via its text path.
#[test]
fn enc_character_is_none() {
    assert_eq!(
        enc(
            &character("a"),
            ModifiersState::empty(),
            KeyEncodeFlags::empty()
        ),
        None
    );
}

// --- encode_terminal_input: frontend-facing key event policy. ---

#[test]
fn terminal_input_suppresses_release_without_event_type_reporting() {
    let got = encode_terminal_input(
        &released(NamedKey::Escape),
        ModifiersState::empty(),
        KeyEncodeFlags::DISAMBIGUATE_ESC_CODES,
        None,
    );

    assert_eq!(got, None);
}

#[test]
fn terminal_input_reports_release_when_event_type_reporting_is_enabled() {
    let got = encode_terminal_input(
        &released(NamedKey::Escape),
        ModifiersState::empty(),
        KeyEncodeFlags::DISAMBIGUATE_ESC_CODES | KeyEncodeFlags::REPORT_EVENT_TYPES,
        None,
    );

    assert_eq!(got.as_deref(), Some(&b"\x1b[27;1:3u"[..]));
}

#[test]
fn terminal_input_uses_text_fallback_for_printable_press() {
    let got = encode_terminal_input(
        &character("a"),
        ModifiersState::empty(),
        KeyEncodeFlags::empty(),
        Some("a"),
    );

    assert_eq!(got.as_deref(), Some(&b"a"[..]));
}

// --- terminal protocol helpers used by frontend input adapters. ---

#[test]
fn bracket_paste_wraps_only_when_active() {
    assert_eq!(bracket_paste(b"ls -la", false), b"ls -la".to_vec());
    assert_eq!(
        bracket_paste(b"ls -la", true),
        b"\x1b[200~ls -la\x1b[201~".to_vec()
    );
    assert_eq!(bracket_paste(b"", true), b"\x1b[200~\x1b[201~".to_vec());
}

#[test]
fn mouse_report_sgr_and_legacy() {
    assert_eq!(
        encode_mouse_report(true, 0, 0, true, 4, 2).unwrap(),
        b"\x1b[<0;5;3M"
    );
    assert_eq!(
        encode_mouse_report(true, 0, 0, false, 4, 2).unwrap(),
        b"\x1b[<0;5;3m"
    );
    assert_eq!(
        encode_mouse_report(true, 64, 16, true, 0, 0).unwrap(),
        b"\x1b[<80;1;1M"
    );
    assert_eq!(
        encode_mouse_report(false, 0, 0, true, 0, 0).unwrap(),
        &[0x1b, b'[', b'M', 32, 33, 33]
    );
    assert_eq!(
        encode_mouse_report(false, 0, 0, false, 0, 0).unwrap(),
        &[0x1b, b'[', b'M', 35, 33, 33]
    );
    assert!(encode_mouse_report(false, 0, 0, true, 223, 0).is_none());
}

#[test]
fn consumed_key_up_is_suppressed_even_under_event_types() {
    assert_eq!(key_up_action(true, true), KeyUpAction::Suppress);
    assert_eq!(key_up_action(false, true), KeyUpAction::EncodeRelease);
}

#[test]
fn unconsumed_key_up_reports_release_only_under_event_types() {
    assert_eq!(key_up_action(false, true), KeyUpAction::EncodeRelease);
    assert_eq!(key_up_action(false, false), KeyUpAction::Fallthrough);
}
