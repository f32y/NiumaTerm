use crate::protocol::pairing::*;

fn sample() -> PairingCode {
    PairingCode {
        relay_url: "wss://relay.example.com/ws".into(),
        host_id: derive_host_id(&[7u8; 32]),
        host_public_key: [7u8; 32],
        token: [0xAB; 16],
    }
}

#[test]
fn roundtrip() {
    let code = sample();
    assert_eq!(PairingCode::decode(&code.encode()).unwrap(), code);
}

#[test]
fn tolerates_whitespace_and_case() {
    let code = sample();
    let mangled = format!("  {}  ", code.encode().to_lowercase());
    assert_eq!(PairingCode::decode(&mangled).unwrap(), code);
}

#[test]
fn corrupted_inputs_error_cleanly() {
    let encoded = sample().encode();
    // Truncated payload.
    assert_eq!(
        PairingCode::decode(&encoded[..encoded.len() - 10]),
        Err(PairingCodeError::Malformed)
    );
    // Wrong prefix.
    assert_eq!(
        PairingCode::decode("XYZ-ABCDEF"),
        Err(PairingCodeError::MissingPrefix)
    );
    // Illegal base32 characters.
    assert_eq!(
        PairingCode::decode("NMT1-!!!!"),
        Err(PairingCodeError::InvalidBase32)
    );
    assert_eq!(
        PairingCode::decode(""),
        Err(PairingCodeError::MissingPrefix)
    );
}

#[test]
fn host_id_is_stable_hex() {
    let id = derive_host_id(&[1u8; 32]);
    assert_eq!(id.len(), 16);
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(id, derive_host_id(&[1u8; 32]));
    assert_ne!(id, derive_host_id(&[2u8; 32]));
}
