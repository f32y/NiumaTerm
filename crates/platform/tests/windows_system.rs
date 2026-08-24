#![cfg(windows)]

use nmt_platform::windows::data_protection;

#[test]
fn protected_data_roundtrips_for_current_user() {
    let secret = b"thirty-two bytes of key material";
    let encrypted = data_protection::protect(secret).expect("protect data");
    assert_ne!(encrypted, secret, "encrypted data must not be plaintext");
    assert_eq!(
        data_protection::unprotect(&encrypted).expect("unprotect data"),
        secret
    );
}

#[test]
fn protected_data_rejects_tampering() {
    let mut encrypted = data_protection::protect(b"secret").expect("protect data");
    let last = encrypted.len() - 1;
    encrypted[last] ^= 0xFF;
    assert!(data_protection::unprotect(&encrypted).is_err());
}
