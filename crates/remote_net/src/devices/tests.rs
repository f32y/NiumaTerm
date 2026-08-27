use std::{env, process};

use crate::devices::*;

#[test]
fn add_contains_remove_persist() {
    let dir = env::temp_dir().join(format!("nmt-dev-test-{}", process::id()));
    let path = dir.join("authorized_devices.json");
    let key = [7u8; 32];

    let mut devices = AuthorizedDevices::load(path.clone()).unwrap();
    assert!(!devices.contains(&key));
    devices.add("laptop", &key).unwrap();
    assert!(devices.contains(&key));

    // Persisted: a fresh load sees the entry.
    let reloaded = AuthorizedDevices::load(path.clone()).unwrap();
    assert!(reloaded.contains(&key));
    assert_eq!(reloaded.entries()[0].name, "laptop");

    assert!(devices.remove(&hex_encode(&key)).unwrap());
    assert!(!devices.contains(&key));
    let reloaded = AuthorizedDevices::load(path).unwrap();
    assert!(!reloaded.contains(&key));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn hex_roundtrip_and_rejects_garbage() {
    let key = [0u8, 15, 16, 255];
    assert_eq!(hex_encode(&key), "000f10ff");
    assert_eq!(hex_decode(&hex_encode(&key)), Some(key.to_vec()));
    assert_eq!(hex_decode("abc"), None, "odd length");
    assert_eq!(hex_decode("zz"), None, "non-hex digits");
}
