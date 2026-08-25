use std::path::PathBuf;
use std::{env, fs, process};

use crate::update::InstallError;
use crate::update::install::{InstallPlan, apply, differing};

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
        ("NmtShellExtension.dll", Some("a0e2c9f"), Some("a0e2c9f")),
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
        ("NmtAgentHook.exe", None, None),
        ("conpty.dll", None, Some("1.24.0")),
    ]);

    assert_eq!(
        differing(&versions),
        ["NiumaTerm.exe", "NmtAgentHook.exe", "conpty.dll"]
    );
}

fn scratch(name: &str) -> PathBuf {
    let directory = env::temp_dir().join(format!("nmt-install-plan-{}-{name}", process::id()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap();
    directory
}

#[test]
fn selected_names_are_exposed_before_the_swap() {
    let plan = InstallPlan {
        names: differing(&versions([
            ("NiumaTerm.exe", Some("v1.3.0"), Some("v1.2.0")),
            (
                "NmtShellExtension.dll",
                Some("new-shell"),
                Some("old-shell"),
            ),
            ("conpty.dll", Some("1.24.0"), Some("1.24.0")),
        ])),
    };

    assert!(!plan.is_empty());
    assert!(plan.contains("NiumaTerm.exe"));
    assert!(plan.contains("NmtShellExtension.dll"));
    assert!(!plan.contains("conpty.dll"));
}

#[test]
fn applying_a_plan_reports_a_missing_staged_file() {
    let staging = scratch("staging");
    let install = scratch("install");
    let plan = InstallPlan {
        names: vec!["NmtShellExtension.dll"],
    };

    assert_eq!(
        apply(&staging, &install, &plan),
        Err(InstallError::NotWritable)
    );
}
