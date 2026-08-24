use std::path::PathBuf;
use std::{env, fs, process};

use crate::windows::self_update::{INCOMING_SUFFIX, PREVIOUS_SUFFIX, ReplaceFilesError, swap};

fn scratch(name: &str) -> PathBuf {
    let directory = env::temp_dir().join(format!("nmt-update-{}-{name}", process::id()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("create the scratch directory");
    directory
}

#[test]
fn completed_swap_leaves_old_files_renamed_aside() {
    let install = scratch("swap");
    let names = ["one.txt", "two.txt"];

    for name in names {
        fs::write(install.join(name), "installed").unwrap();
        fs::write(install.join(format!("{name}{INCOMING_SUFFIX}")), "staged").unwrap();
    }

    swap(&install, &names).expect("every incoming file is in place");

    for name in names {
        assert_eq!(fs::read_to_string(install.join(name)).unwrap(), "staged");
        assert_eq!(
            fs::read_to_string(install.join(format!("{name}{PREVIOUS_SUFFIX}"))).unwrap(),
            "installed"
        );
        assert!(!install.join(format!("{name}{INCOMING_SUFFIX}")).exists());
    }
}

#[test]
fn failed_swap_restores_moved_files() {
    let install = scratch("rollback");
    let names = ["one.txt", "two.txt", "three.txt"];

    for name in names {
        fs::write(install.join(name), "installed").unwrap();
        fs::write(install.join(format!("{name}{INCOMING_SUFFIX}")), "staged").unwrap();
    }
    fs::remove_file(install.join(format!("three.txt{INCOMING_SUFFIX}"))).unwrap();

    assert!(matches!(
        swap(&install, &names),
        Err(ReplaceFilesError::Replace { .. })
    ));

    for name in names {
        assert_eq!(fs::read_to_string(install.join(name)).unwrap(), "installed");
        assert!(!install.join(format!("{name}{PREVIOUS_SUFFIX}")).exists());
        assert!(!install.join(format!("{name}{INCOMING_SUFFIX}")).exists());
    }
}
