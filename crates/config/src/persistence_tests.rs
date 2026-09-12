use std::path::PathBuf;
use std::process::Command;
use std::{env, fs, io};

use tempfile::tempdir;

use crate::persistence::update;

#[test]
fn failed_edit_preserves_the_original_file() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("config.toml");

    fs::write(&path, "original").unwrap();

    let error = update(&path, |_| Err(io::Error::other("cannot encode"))).unwrap_err();

    assert_eq!(error.to_string(), "cannot encode");
    assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
}

#[test]
fn unreadable_content_never_reaches_the_edit() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("config.toml");

    fs::write(&path, [0xff, 0xfe]).unwrap();

    let error = update(&path, |_| panic!("read failure must stop the update")).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(fs::read(&path).unwrap(), [0xff, 0xfe]);
}

#[test]
fn process_writer() {
    let Some(path) = env::var_os("NMT_CONFIG_TEST_UPDATE_PATH") else {
        return;
    };

    let path: PathBuf = path.into();

    for _ in 0..25 {
        update(&path, |content| {
            let count: usize = content.unwrap_or("0").parse().unwrap();

            Ok((count + 1).to_string())
        })
        .unwrap();
    }
}

#[test]
fn concurrent_processes_preserve_every_update() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("counter");
    let executable = env::current_exe().unwrap();
    let mut children = Vec::new();

    for _ in 0..4 {
        children.push(
            Command::new(&executable)
                .args(["--exact", "persistence::persistence_tests::process_writer"])
                .env("NMT_CONFIG_TEST_UPDATE_PATH", &path)
                .spawn()
                .unwrap(),
        );
    }

    for mut child in children {
        assert!(child.wait().unwrap().success());
    }

    assert_eq!(fs::read_to_string(&path).unwrap(), "100");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
}
