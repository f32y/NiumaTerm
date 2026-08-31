use std::path::PathBuf;
use std::{env, fs, process};

use crate::update::InstallError;
use crate::update::install::{APP_EXE, InstallPlan, apply, differing, install_additions, plan};

fn versions(
    entries: [(&str, Option<&str>, Option<&str>); 3],
) -> Vec<(String, Option<String>, Option<String>)> {
    entries
        .into_iter()
        .map(|(name, staged, installed)| {
            (
                name.to_owned(),
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
        names: vec!["NmtShellExtension.dll".to_owned()],
    };

    assert_eq!(
        apply(&staging, &install, &plan),
        Err(InstallError::NotWritable)
    );
}

#[test]
fn a_file_no_build_knows_about_is_planned_from_the_package() {
    let staging = scratch("unlisted-staging");
    let install = scratch("unlisted-install");
    // The build that performs a swap is the one being replaced, so a release
    // adding a file can only install it if the list comes from the package.
    fs::write(staging.join("nmt_later_addition.dll"), b"a later addition").unwrap();

    assert!(plan(&staging, &install).contains("nmt_later_addition.dll"));
}

#[test]
fn a_new_syntax_language_dll_is_selected_for_installation() {
    let staging = scratch("syntax-staging");
    let install = scratch("syntax-install");
    fs::write(staging.join("tree_sitter.dll"), b"new syntax languages").unwrap();

    assert!(plan(&staging, &install).contains("tree_sitter.dll"));
}

/// A file that carries a readable version resource, standing in for a build of
/// the application: what attribution compares is a real resource, so a file
/// written here cannot exercise it. A system DLL is the one such file present
/// on every machine these tests run on.
fn versioned_binary() -> PathBuf {
    PathBuf::from(env::var_os("SystemRoot").expect("SystemRoot is set on Windows"))
        .join("System32")
        .join("kernel32.dll")
}

#[test]
fn a_file_the_installation_lacks_is_taken_from_the_staged_package() {
    let package = scratch("addition-package");
    let install = scratch("addition-install");
    fs::copy(versioned_binary(), package.join(APP_EXE)).unwrap();
    fs::copy(versioned_binary(), install.join(APP_EXE)).unwrap();
    fs::write(package.join("tree_sitter.dll"), b"staged languages").unwrap();

    install_additions(&package, &install);

    assert_eq!(
        fs::read(install.join("tree_sitter.dll")).unwrap(),
        b"staged languages"
    );
}

#[test]
fn an_installed_file_is_kept_over_the_staged_copy() {
    let package = scratch("kept-package");
    let install = scratch("kept-install");
    fs::copy(versioned_binary(), package.join(APP_EXE)).unwrap();
    fs::copy(versioned_binary(), install.join(APP_EXE)).unwrap();
    fs::write(package.join("conpty.dll"), b"staged conpty").unwrap();
    fs::write(install.join("conpty.dll"), b"installed conpty").unwrap();

    install_additions(&package, &install);

    assert_eq!(
        fs::read(install.join("conpty.dll")).unwrap(),
        b"installed conpty"
    );
}

#[test]
fn a_package_that_is_not_the_installed_release_contributes_nothing() {
    let package = scratch("foreign-package");
    let install = scratch("foreign-install");
    // An executable whose version cannot be read names no release, so the
    // package holding it cannot be shown to be the one installed.
    fs::write(package.join(APP_EXE), b"not an executable").unwrap();
    fs::copy(versioned_binary(), install.join(APP_EXE)).unwrap();
    fs::write(package.join("tree_sitter.dll"), b"staged languages").unwrap();

    install_additions(&package, &install);

    assert!(!install.join("tree_sitter.dll").exists());
}
