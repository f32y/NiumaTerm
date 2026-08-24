use std::{env, fs, process};

use crate::keys::load_or_create_keypair;

#[test]
fn keypair_persists_across_loads() {
    let dir = env::temp_dir().join(format!("nmt-key-test-{}", process::id()));
    let path = dir.join("host-key.json");
    let first = load_or_create_keypair(&path).expect("create");
    let second = load_or_create_keypair(&path).expect("load");
    assert_eq!(first.public, second.public);
    assert_eq!(first.private, second.private);
    fs::remove_dir_all(&dir).ok();
}
