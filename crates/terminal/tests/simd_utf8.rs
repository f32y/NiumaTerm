use nmt_terminal::simd_utf8::*;

#[test]
fn test_valid_utf8() {
    let bytes = b"Hello, \xE2\x9D\xA4\xEF\xB8\x8F UTF-8!";
    let result = from_utf8_fast(bytes).unwrap();
    assert_eq!(result, "Hello, ❤️ UTF-8!");
}

#[test]
fn test_invalid_utf8() {
    let bytes = b"Hello, \xFF invalid UTF-8!";
    assert!(from_utf8_fast(bytes).is_err());

    let result = from_utf8_lossy_fast(bytes);
    assert!(result.contains("Hello"));
    assert!(result.contains("invalid UTF-8!"));
}

#[test]
fn test_compat_error_info() {
    let bytes = b"Valid\xFF\xFEInvalid";
    let err = from_utf8_compat(bytes).unwrap_err();
    assert!(err.to_string().contains("invalid utf-8"));
    assert_eq!(err.valid_up_to(), 5);
    assert_eq!(err.error_len(), Some(1));
}

#[test]
fn truncated_utf8_returns_none() {
    // Lead byte for a 4-byte sequence, only 2 bytes provided.
    let bytes = b"\xF0\x9F";
    let err = validate(bytes).unwrap_err();
    assert_eq!(err.valid_up_to(), 0);
    assert_eq!(err.error_len(), None);
}

#[test]
fn truncated_with_bad_continuation_returns_some() {
    // Lead byte for 3-byte sequence, second byte is invalid continuation.
    let bytes = b"\xE0\x20";
    let err = validate(bytes).unwrap_err();
    assert_eq!(err.valid_up_to(), 0);
    assert_eq!(err.error_len(), Some(1));
}

#[test]
fn complete_invalid_sequence_returns_some_len() {
    // Surrogate codepoint encoded in UTF-8 (3 bytes) — valid bytes,
    // invalid Unicode value.
    let bytes = b"\xED\xA0\x80"; // U+D800, surrogate
    let err = validate(bytes).unwrap_err();
    assert_eq!(err.valid_up_to(), 0);
    assert_eq!(err.error_len(), Some(3));
}
