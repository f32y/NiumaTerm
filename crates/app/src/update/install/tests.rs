use std::path::PathBuf;
use std::{env, fs, process};

use crate::update::InstallError;
use crate::update::install::{INCOMING_SUFFIX, PREVIOUS_SUFFIX, differing, swap};

fn scratch(name: &str) -> PathBuf {
    let directory = env::temp_dir().join(format!("nmt-update-{}-{name}", process::id()));

    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("create the scratch directory");

    directory
}

fn versions(
    entries: [(&'static str, Option<&str>, Option<&str>); 3],
) -> Vec<(&'static str, Option<String>, Option<String>)> {
    entries
        .into_iter()
        .map(|(name, staged, installed)| {
            (
                name,
                staged.map(str::to_owned),
                installed.map(str::to_owned),
            )
        })
        .collect()
}

#[test]
fn only_files_whose_version_moved_are_replaced() {
    let versions = versions([
        ("NiumaTerm.exe", Some("v1.3.0"), Some("v1.2.0")),
        // The release advanced, but this file's own revision did not, which is
        // the whole point of giving it a different key to be compared by.
        ("shell_extension.dll", Some("a0e2c9f"), Some("a0e2c9f")),
        ("conpty.dll", Some("1.24.0"), Some("1.24.0")),
    ]);

    assert_eq!(differing(&versions), ["NiumaTerm.exe"]);
}

#[test]
fn a_version_that_cannot_be_read_counts_as_a_difference() {
    // In order: a file the installation does not have yet, a file that carries
    // no version resource on either side, and one whose staged copy could not
    // be read. None of them may be assumed to be current.
    let versions = versions([
        ("NiumaTerm.exe", Some("v1.3.0"), None),
        ("NiumaTermHook.exe", None, None),
        ("conpty.dll", None, Some("1.24.0")),
    ]);

    assert_eq!(
        differing(&versions),
        ["NiumaTerm.exe", "NiumaTermHook.exe", "conpty.dll"]
    );
}

#[test]
fn a_completed_swap_leaves_the_old_files_renamed_aside() {
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
fn a_swap_that_fails_part_way_puts_back_what_it_moved() {
    let install = scratch("rollback");
    let names = ["one.txt", "two.txt", "three.txt"];

    for name in names {
        fs::write(install.join(name), "installed").unwrap();
        fs::write(install.join(format!("{name}{INCOMING_SUFFIX}")), "staged").unwrap();
    }

    // The third file has nothing to move into place, which is the failure the
    // first two have to be undone for.
    fs::remove_file(install.join(format!("three.txt{INCOMING_SUFFIX}"))).unwrap();

    assert_eq!(swap(&install, &names), Err(InstallError::Replace));

    for name in names {
        assert_eq!(fs::read_to_string(install.join(name)).unwrap(), "installed");
        assert!(!install.join(format!("{name}{PREVIOUS_SUFFIX}")).exists());
        // An abandoned swap also takes its copies with it, so a later attempt
        // never finds a half-fetched package waiting in the installation.
        assert!(!install.join(format!("{name}{INCOMING_SUFFIX}")).exists());
    }
}
