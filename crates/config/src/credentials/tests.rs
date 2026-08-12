use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::credentials::{PREFIX, decrypt, encrypt};

const URL: &str = "https://proxy.example.com/v1";
const KEY: &str = "sk-test-1234";

#[test]
fn round_trip_restores_both_values() {
    let stored = encrypt(URL, KEY).unwrap();
    assert!(stored.starts_with(PREFIX));
    assert_eq!(
        decrypt(&stored).unwrap(),
        (URL.to_string(), KEY.to_string())
    );
}

#[test]
fn round_trip_restores_empty_values() {
    let stored = encrypt("", "").unwrap();
    assert_eq!(decrypt(&stored).unwrap(), (String::new(), String::new()));
}

/// Encrypted with the committed application key. A failure here means the
/// embedded key or the storage format changed, which would make every
/// previously saved profile unreadable.
#[test]
fn known_vector_still_decrypts() {
    let stored = "aes256gcm-v1:XTOQqFXclYE3lA7JrG6QTLzSvW5PAnErU0mzmJkQ4seB/HSp/BHCUgPljAte41VsTo3NWcJ2CP24FcStxnQzFqFSW2qgJscuPBWyDR05KW4Xs4n/33eNighnDv1olKgp9DWD";
    assert_eq!(decrypt(stored).unwrap(), (URL.to_string(), KEY.to_string()));
}

#[test]
fn repeated_encryption_produces_different_output() {
    let first = encrypt(URL, KEY).unwrap();
    let second = encrypt(URL, KEY).unwrap();
    assert_ne!(first, second);
    assert_eq!(decrypt(&first).unwrap(), decrypt(&second).unwrap());
}

#[test]
fn modified_data_is_rejected() {
    let stored = encrypt(URL, KEY).unwrap();
    let mut bytes = BASE64.decode(stored.strip_prefix(PREFIX).unwrap()).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    let modified = format!("{PREFIX}{}", BASE64.encode(bytes));

    let err = decrypt(&modified).unwrap_err();
    assert!(err.contains("authentication"), "{err}");
}

#[test]
fn short_input_is_rejected() {
    let stored = format!("{PREFIX}{}", BASE64.encode([0u8; 27]));
    let err = decrypt(&stored).unwrap_err();
    assert!(err.contains("too short"), "{err}");
}

#[test]
fn malformed_base64_is_rejected() {
    let err = decrypt("aes256gcm-v1:!!!not-base64!!!").unwrap_err();
    assert!(err.contains("Base64"), "{err}");
}

#[test]
fn unknown_version_is_rejected() {
    for stored in ["aes256gcm-v2:AAAA", "plain:AAAA", ""] {
        let err = decrypt(stored).unwrap_err();
        assert!(err.contains("unsupported"), "{err}");
    }
}
