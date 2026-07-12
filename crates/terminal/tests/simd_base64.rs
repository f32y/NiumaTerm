use nmt_terminal::simd_base64::*;

#[test]
fn decode_empty() {
    assert_eq!(decode(b"").unwrap(), b"");
}

#[test]
fn decode_basic_padded() {
    assert_eq!(decode(b"aGVsbG8gd29ybGQ=").unwrap(), b"hello world");
}

#[test]
fn decode_basic_unpadded_via_no_pad() {
    assert_eq!(decode_no_pad(b"aGVsbG8gd29ybGQ").unwrap(), b"hello world");
}

#[test]
fn decode_invalid() {
    assert!(decode(b"not!valid#base64").is_none());
}

#[test]
fn decode_round_trip_kitty_chunk() {
    // ~4 KB payload typical of kitty graphics.
    let bytes: Vec<u8> = (0..4096).map(|i| (i & 0xff) as u8).collect();
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    let encoded = STANDARD.encode(&bytes);
    let decoded = decode(encoded.as_bytes()).unwrap();
    assert_eq!(decoded, bytes);
}
