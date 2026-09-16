use std::fs;

use nmt_config::update::{UpdateChannel, UpdateConfig};
use tempfile::{TempDir, tempdir};

use crate::windows::install::{Installation, SHELL_EXTENSION_DLL};
use crate::windows::releases::{CheckedRelease, Release};
use crate::windows::{InstallAction, InstallError, Status, Updater};

fn release() -> Release {
    Release {
        label: "v2.0.0".into(),
        page_url: String::new(),
        assets: Vec::new(),
        published: None,
    }
}

fn staged_update() -> (Updater, TempDir, TempDir) {
    let staging = tempdir().unwrap();
    let install = tempdir().unwrap();

    fs::write(staging.path().join(SHELL_EXTENSION_DLL), "new dll").unwrap();

    let mut updater = Updater::new(
        "v1.0.0",
        install.path().to_path_buf(),
        staging.path(),
        UpdateConfig::default(),
        true,
    );

    assert!(updater.begin_check().is_some());
    assert!(updater.finish_check(CheckedRelease {
        channel: UpdateChannel::Stable,
        status: Status::Available(release()),
    }));
    assert!(updater.begin_install().is_some());

    assert_eq!(
        updater.finish_download(Ok(Installation::new(
            release(),
            staging.path().to_path_buf(),
            install.path().to_path_buf(),
            true,
        ))),
        InstallAction::InspectFileUsers
    );

    (updater, staging, install)
}

#[test]
fn cancelling_a_staged_update_replaces_nothing_and_restores_availability() {
    let (mut updater, staging, install) = staged_update();

    assert!(updater.cancel_install());
    assert_eq!(updater.status(), &Status::Available(release()));
    assert!(fs::read_dir(install.path()).unwrap().next().is_none());
    assert!(staging.path().join(SHELL_EXTENSION_DLL).exists());
    assert!(!updater.cancel_install());
}

#[test]
fn continuing_applies_the_plan_before_reporting_relaunch_failure() {
    let (mut updater, _staging, install) = staged_update();

    assert_eq!(updater.continue_install(), InstallAction::Relaunch);
    assert_eq!(
        fs::read_to_string(install.path().join(SHELL_EXTENSION_DLL)).unwrap(),
        "new dll"
    );

    // The installation deliberately has no executable, so relaunch fails
    // without starting another process or discarding the installed DLL.
    assert!(!updater.relaunch());
    assert_eq!(
        updater.status(),
        &Status::InstallFailed(InstallError::Relaunch)
    );
    assert!(!updater.cancel_install());
}

#[test]
fn switching_channels_rejects_a_late_check_result() {
    let root = tempdir().unwrap();

    let mut updater = Updater::new(
        "v1.0.0",
        root.path().to_path_buf(),
        root.path(),
        UpdateConfig::default(),
        false,
    );

    assert!(updater.begin_check().is_some());
    assert!(updater.set_settings(UpdateConfig {
        check_updates: true,
        channel: UpdateChannel::Nightly,
    }));
    assert_eq!(updater.status(), &Status::Unknown);
    assert!(updater.begin_check().is_some());
    assert!(!updater.finish_check(CheckedRelease {
        channel: UpdateChannel::Stable,
        status: Status::Available(release()),
    }));
    assert_eq!(updater.status(), &Status::Checking);
    assert!(updater.finish_check(CheckedRelease {
        channel: UpdateChannel::Nightly,
        status: Status::NothingPublished,
    }));
    assert_eq!(updater.status(), &Status::NothingPublished);
}

#[test]
fn settings_changes_keep_an_install_on_its_original_release() {
    let (mut updater, _staging, _install) = staged_update();

    assert!(!updater.set_settings(UpdateConfig {
        check_updates: false,
        channel: UpdateChannel::Nightly,
    }));
    assert!(updater.begin_check().is_none());
    assert!(!updater.automatic_checks_enabled());
    assert_eq!(updater.continue_install(), InstallAction::Relaunch);
    assert_eq!(updater.status(), &Status::Installing(release()));
}

#[test]
fn a_test_instance_disables_automatic_checks_but_allows_manual_checks() {
    let root = tempdir().unwrap();

    let mut updater = Updater::new(
        "v1.0.0",
        root.path().to_path_buf(),
        root.path(),
        UpdateConfig {
            check_updates: false,
            channel: UpdateChannel::Stable,
        },
        true,
    );

    assert!(!updater.set_settings(UpdateConfig::default()));
    assert!(!updater.automatic_checks_enabled());
    assert!(updater.begin_check().is_some());
    assert_eq!(updater.status(), &Status::Checking);
}
