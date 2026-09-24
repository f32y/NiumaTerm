use std::path::PathBuf;
use std::{env, fs, process};

use crate::windows::self_update::{
    INCOMING_SUFFIX, PREVIOUS_SUFFIX, ReplaceFilesError, discard_previous, swap, undo,
};

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

#[test]
fn failed_undo_reports_error_and_keeps_recovery_copies() {
    let install = scratch("undo-failure");

    fs::write(install.join("one.txt"), "replacement").unwrap();

    fs::write(
        install.join(format!("one.txt{PREVIOUS_SUFFIX}")),
        "original",
    )
    .unwrap();

    fs::create_dir(install.join(format!("one.txt{INCOMING_SUFFIX}"))).unwrap();

    let errors = undo(&install, &[("one.txt", true)]);

    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("one.txt"));

    discard_previous(&install);

    assert_eq!(
        fs::read_to_string(install.join(format!("one.txt{PREVIOUS_SUFFIX}"))).unwrap(),
        "original"
    );
    assert_eq!(
        fs::read_to_string(install.join("one.txt")).unwrap(),
        "replacement"
    );
}

#[test]
fn startup_cleanup_keeps_the_only_copy_of_a_missing_target() {
    let install = scratch("missing-target");
    let previous = install.join(format!("one.txt{PREVIOUS_SUFFIX}"));

    fs::write(&previous, "original").unwrap();

    discard_previous(&install);

    assert_eq!(fs::read_to_string(&previous).unwrap(), "original");

    fs::write(install.join("one.txt"), "replacement").unwrap();

    discard_previous(&install);

    assert!(!previous.exists());
}
