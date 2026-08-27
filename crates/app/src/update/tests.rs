use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{env, fs, process, slice};

use gpui::TestAppContext;
use nmt_config::update::UpdateChannel;
use nmt_platform::windows::restart_manager::{
    AffectedApplication, ApplicationKind, ApplicationStatus, FileUsage, Operation, RebootReasons,
    RestartManagerError,
};
use nmt_version::Version;
use parking_lot::Mutex;

use crate::update::file_users::display_names;
use crate::update::releases::{CheckError, Release, select, select_latest, supersedes};
use crate::update::{
    AppUpdate, ClosePreparation, FileUsePromptReason, FileUserSession, FileUserSessionSource,
    InstallError, PendingInstall, Status, cancel_install, classify_file_usage, continue_install,
    install, prepare_close_with, recovery_application_names, status,
};

#[derive(Clone)]
struct ScriptedSessionSource {
    state: Arc<Mutex<CloseState>>,
}

struct ScriptedSession {
    state: Arc<Mutex<CloseState>>,
}

struct CloseState {
    usage: VecDeque<Result<FileUsage, u32>>,
    shutdown_error: Option<u32>,
    restart_error: Option<u32>,
    events: Vec<&'static str>,
}

impl FileUserSessionSource for ScriptedSessionSource {
    type Session = ScriptedSession;

    fn open(&self, _path: &Path) -> Result<Self::Session, RestartManagerError> {
        self.state.lock().events.push("open");
        Ok(ScriptedSession {
            state: self.state.clone(),
        })
    }
}

impl FileUserSession for ScriptedSession {
    fn file_usage(&self) -> Result<FileUsage, RestartManagerError> {
        let mut state = self.state.lock();
        state.events.push("list");
        state
            .usage
            .pop_front()
            .unwrap()
            .map_err(|code| RestartManagerError::Windows {
                operation: Operation::ListApplications,
                code,
            })
    }

    fn shutdown(&self) -> Result<(), RestartManagerError> {
        let mut state = self.state.lock();
        state.events.push("shutdown");
        match state.shutdown_error {
            Some(code) => Err(RestartManagerError::Windows {
                operation: Operation::ShutdownApplications,
                code,
            }),
            None => Ok(()),
        }
    }

    fn restart(&self) -> Result<(), RestartManagerError> {
        let mut state = self.state.lock();
        state.events.push("restart");
        match state.restart_error {
            Some(code) => Err(RestartManagerError::Windows {
                operation: Operation::RestartApplications,
                code,
            }),
            None => Ok(()),
        }
    }
}

fn version(label: &str) -> Version {
    Version::parse(label).expect("test labels are well formed")
}

/// A candidate the channel published, with no date. Only a comparison across
/// channels reads the date, so every same-channel case supplies none.
fn undated(label: &str) -> Release {
    Release {
        label: label.to_owned(),
        page_url: String::new(),
        assets: Vec::new(),
        published: None,
    }
}

fn published_on(label: &str, published: u32) -> Release {
    Release {
        published: Some(published),
        ..undated(label)
    }
}

fn application(name: &str, process_id: u32, restartable: bool) -> AffectedApplication {
    AffectedApplication {
        name: name.to_owned(),
        service_name: None,
        process_id,
        kind: ApplicationKind::Explorer,
        status: ApplicationStatus::from_bits(0),
        terminal_session_id: Some(1),
        restartable,
    }
}

fn scratch(name: &str) -> PathBuf {
    let directory = env::temp_dir().join(format!("nmt-update-flow-{}-{name}", process::id()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap();
    directory
}

#[test]
fn a_higher_release_supersedes_a_lower_one() {
    assert!(supersedes(&version("v1.2.0"), &undated("v1.3.0")));
    assert!(supersedes(&version("v1.2.0"), &undated("v1.2.1")));
    assert!(supersedes(&version("v1.9.0"), &undated("v2.0.0")));

    assert!(!supersedes(&version("v1.3.0"), &undated("v1.2.0")));
    assert!(!supersedes(&version("v1.2.0"), &undated("v1.2.0")));
    // Component order, which a lexical comparison of the labels would get
    // wrong once a number reaches two digits.
    assert!(!supersedes(&version("v1.10.0"), &undated("v1.9.0")));
    assert!(supersedes(&version("v1.9.0"), &undated("v1.10.0")));
}

#[test]
fn a_later_nightly_supersedes_an_earlier_one() {
    let installed = version("nightly-20260821-7567b41");

    assert!(supersedes(&installed, &undated("nightly-20260822-aaaaaaa")));
    assert!(!supersedes(
        &installed,
        &undated("nightly-20260820-aaaaaaa")
    ));
    assert!(!supersedes(
        &installed,
        &undated("nightly-20260821-7567b41")
    ));

    // Two revisions dated the same day: the published list is ordered by when
    // each release was cut, so a different one reached from that list is the
    // newer build rather than an ambiguous one.
    assert!(supersedes(&installed, &undated("nightly-20260821-aaaaaaa")));
}

#[test]
fn a_tag_that_cannot_be_read_is_still_offered() {
    assert!(supersedes(&version("v1.2.0"), &undated("build-4")));
}

#[test]
fn a_release_older_than_a_running_nightly_is_not_offered() {
    let installed = version("nightly-20260822-7567b41");

    // The case a locally built binary lands in: the release predates the
    // revision it was built from, so its lower number is not a downgrade to
    // offer.
    assert!(!supersedes(&installed, &published_on("v1.2.3", 20260814)));
    // A release cut the same day is the ambiguous case, and it is the one a
    // build made from that day's tree keeps being offered.
    assert!(!supersedes(&installed, &published_on("v1.2.3", 20260822)));
    // A later day carries revisions the nightly cannot have.
    assert!(supersedes(&installed, &published_on("v1.2.3", 20260823)));
    // With nothing to place it by, it cannot be shown to be ahead.
    assert!(!supersedes(&installed, &undated("v1.2.3")));
}

#[test]
fn a_nightly_is_offered_to_a_release_build() {
    // Moving to the nightly channel needs no publishing date: that channel is
    // only ever cut from the tip.
    assert!(supersedes(
        &version("v1.2.0"),
        &undated("nightly-20260821-7567b41")
    ));
}

/// Shaped like the releases page: newest first, with the entries this
/// repository actually carries from before the version naming settled.
const RELEASES: &str = r#"[
    { "tag_name": "nightly-20260822-bbbbbbb", "html_url": "https://example.invalid/n2",
      "draft": false, "prerelease": true },
    { "tag_name": "v1.4.0", "html_url": "https://example.invalid/draft",
      "draft": true, "prerelease": false },
    { "tag_name": "v1.3.0", "html_url": "https://example.invalid/r3",
      "draft": false, "prerelease": false },
    { "tag_name": "nightly-20260820-aaaaaaa", "html_url": "https://example.invalid/n1",
      "draft": false, "prerelease": true },
    { "tag_name": "build-4", "html_url": "https://example.invalid/b4",
      "draft": false, "prerelease": false },
    { "tag_name": "nightly-20260819", "html_url": "https://example.invalid/old",
      "draft": false, "prerelease": true },
    { "tag_name": "v1.2.0", "html_url": "https://example.invalid/r2",
      "draft": false, "prerelease": false }
]"#;

#[test]
fn each_channel_takes_its_own_newest_entry() {
    let stable = select(RELEASES, UpdateChannel::Stable).unwrap().unwrap();
    let nightly = select(RELEASES, UpdateChannel::Nightly).unwrap().unwrap();

    assert_eq!(stable.label, "v1.3.0");
    assert_eq!(stable.page_url, "https://example.invalid/r3");
    assert_eq!(nightly.label, "nightly-20260822-bbbbbbb");
}

#[test]
fn entries_that_cannot_be_compared_are_skipped() {
    // A draft is not published, `build-4` predates the naming, and
    // `nightly-20260819` predates the revision joining it. None of them can be
    // placed against a running build, so neither channel may offer one.
    let unusable = r#"[
        { "tag_name": "v1.4.0", "html_url": "https://example.invalid/draft",
          "draft": true, "prerelease": false },
        { "tag_name": "build-4", "html_url": "https://example.invalid/b4",
          "draft": false, "prerelease": false },
        { "tag_name": "nightly-20260819", "html_url": "https://example.invalid/old",
          "draft": false, "prerelease": true }
    ]"#;

    assert_eq!(select(unusable, UpdateChannel::Stable).unwrap(), None);
    assert_eq!(select(unusable, UpdateChannel::Nightly).unwrap(), None);
}

#[test]
fn a_prerelease_is_never_offered_as_stable() {
    let prerelease_tagged_as_release = r#"[
        { "tag_name": "v9.9.9", "html_url": "https://example.invalid/pre",
          "draft": false, "prerelease": true }
    ]"#;

    assert_eq!(
        select(prerelease_tagged_as_release, UpdateChannel::Stable).unwrap(),
        None
    );
}

#[test]
fn an_empty_channel_is_reported_as_nothing_found() {
    let only_stable = r#"[
        { "tag_name": "v1.2.0", "html_url": "https://example.invalid/r2",
          "draft": false, "prerelease": false }
    ]"#;

    assert_eq!(select(only_stable, UpdateChannel::Nightly).unwrap(), None);
}

#[test]
fn a_response_that_is_not_the_releases_list_is_rejected() {
    assert_eq!(
        select(
            r#"{"message":"API rate limit exceeded"}"#,
            UpdateChannel::Stable
        ),
        Err(CheckError::Unreadable)
    );
}

#[test]
fn the_latest_endpoint_answers_the_stable_channel() {
    let published = r#"{ "tag_name": "v1.3.0", "html_url": "https://example.invalid/r3",
        "draft": false, "prerelease": false }"#;

    let release = select_latest(published).unwrap().unwrap();
    assert_eq!(release.label, "v1.3.0");
    assert_eq!(release.page_url, "https://example.invalid/r3");

    // The endpoint promises the newest published non-prerelease, not that its
    // tag is one this build can be placed against.
    let predates_the_naming = r#"{ "tag_name": "build-4", "html_url": "https://example.invalid/b4",
        "draft": false, "prerelease": false }"#;
    assert_eq!(select_latest(predates_the_naming).unwrap(), None);

    // GitHub answers 404 for a repository with no full release yet, which
    // reaches this as a body that is not a release.
    assert_eq!(
        select_latest(r#"{"message":"Not Found"}"#),
        Err(CheckError::Unreadable)
    );
}

#[test]
fn a_publishing_timestamp_is_reduced_to_a_comparable_date() {
    let dated = r#"{ "tag_name": "v1.3.0", "html_url": "https://example.invalid/r3",
        "draft": false, "prerelease": false, "published_at": "2026-08-14T09:12:33Z" }"#;
    assert_eq!(
        select_latest(dated).unwrap().unwrap().published,
        Some(20260814)
    );

    // A timestamp in a shape this cannot read leaves the release unplaceable,
    // rather than dated from whatever sat at those offsets.
    let malformed = r#"{ "tag_name": "v1.3.0", "html_url": "https://example.invalid/r3",
        "draft": false, "prerelease": false, "published_at": "20260814T09:12:33Z" }"#;
    assert_eq!(select_latest(malformed).unwrap().unwrap().published, None);

    // A release that was never published carries a null timestamp, and the
    // recorded responses carry none at all.
    let null = r#"{ "tag_name": "v1.3.0", "html_url": "https://example.invalid/r3",
        "draft": false, "prerelease": false, "published_at": null }"#;
    assert_eq!(select_latest(null).unwrap().unwrap().published, None);
}

#[test]
fn file_use_results_distinguish_clear_used_unknown_and_reboot_states() {
    let clear = FileUsage {
        applications: Vec::new(),
        reboot_reasons: RebootReasons::default(),
    };
    assert_eq!(classify_file_usage(Ok(clear)).unwrap(), None);

    let explorer = application("Windows Explorer", 101, true);
    let used = FileUsage {
        applications: vec![explorer.clone()],
        reboot_reasons: RebootReasons::default(),
    };
    let prompt = classify_file_usage(Ok(used)).unwrap().unwrap();
    assert_eq!(prompt.reason, FileUsePromptReason::InUse);
    assert_eq!(prompt.applications, slice::from_ref(&explorer));

    let reboot = FileUsage {
        applications: vec![explorer],
        reboot_reasons: RebootReasons {
            session_mismatch: true,
            ..Default::default()
        },
    };
    assert_eq!(
        classify_file_usage(Ok(reboot)).unwrap().unwrap().reason,
        FileUsePromptReason::RebootRequired
    );

    let error = RestartManagerError::Windows {
        operation: Operation::ListApplications,
        code: 5,
    };
    assert!(matches!(
        classify_file_usage(Err(error)),
        Err(RestartManagerError::Windows {
            operation: Operation::ListApplications,
            code: 5,
        })
    ));
}

#[test]
fn recovery_names_only_applications_that_need_manual_work() {
    let restartable = application("Explorer", 1, true);
    let manual = application("Other host", 2, false);
    let applications = [restartable, manual];

    assert_eq!(
        recovery_application_names(&Ok(()), &applications),
        ["Other host"]
    );
    assert_eq!(
        recovery_application_names(
            &Err(RestartManagerError::Windows {
                operation: Operation::RestartApplications,
                code: 352,
            }),
            &applications,
        ),
        ["Explorer", "Other host"]
    );
}

#[test]
fn duplicate_application_names_include_process_identifiers() {
    assert_eq!(
        display_names(&[
            application("Explorer", 11, true),
            application("Explorer", 12, true),
            application("Other", 13, true),
        ]),
        ["Explorer (PID 11)", "Explorer (PID 12)", "Other"]
    );
}

#[test]
fn close_preparation_uses_a_fresh_session_application_list() {
    let current = application("Current host", 22, true);
    let state = Arc::new(Mutex::new(CloseState {
        usage: VecDeque::from([Ok(FileUsage {
            applications: vec![current.clone()],
            reboot_reasons: RebootReasons::default(),
        })]),
        shutdown_error: None,
        restart_error: None,
        events: Vec::new(),
    }));
    let source = ScriptedSessionSource {
        state: state.clone(),
    };

    let result = prepare_close_with(&source, Path::new(r"C:\NiumaTerm\dll"));

    match result {
        ClosePreparation::Released { applications, .. } => {
            assert_eq!(applications, [current]);
        }
        ClosePreparation::Clear | ClosePreparation::Prompt(_) => panic!("expected shutdown"),
    }
    assert_eq!(state.lock().events, ["open", "list", "shutdown"]);
}

#[test]
fn failed_shutdown_restarts_before_remaining_users_are_reported() {
    let current = application("Current host", 22, true);
    let state = Arc::new(Mutex::new(CloseState {
        usage: VecDeque::from([
            Ok(FileUsage {
                applications: vec![current.clone()],
                reboot_reasons: RebootReasons::default(),
            }),
            Ok(FileUsage {
                applications: vec![current.clone()],
                reboot_reasons: RebootReasons::default(),
            }),
        ]),
        shutdown_error: Some(351),
        restart_error: None,
        events: Vec::new(),
    }));
    let source = ScriptedSessionSource {
        state: state.clone(),
    };

    let result = prepare_close_with(&source, Path::new(r"C:\NiumaTerm\dll"));

    match result {
        ClosePreparation::Prompt(prompt) => {
            assert_eq!(prompt.reason, FileUsePromptReason::RemainingUsers);
            assert_eq!(prompt.applications, [current]);
        }
        ClosePreparation::Clear | ClosePreparation::Released { .. } => {
            panic!("expected remaining users")
        }
    }
    assert_eq!(
        state.lock().events,
        ["open", "list", "shutdown", "restart", "list"]
    );
}

#[gpui::test]
fn cancelling_a_staged_update_replaces_nothing_and_restores_availability(cx: &mut TestAppContext) {
    let staging = scratch("cancel-staging");
    let install_root = scratch("cancel-install");
    let plan = install::plan(&staging, &install_root);
    let release = undated("v2.0.0");
    let window = cx.add_empty_window();

    window.update(|window, cx| {
        cx.set_global(AppUpdate {
            status: Status::AwaitingFileUse(release.clone()),
            testing: true,
            pending: Some(PendingInstall {
                release: release.clone(),
                staged: staging.clone(),
                install: install_root.clone(),
                plan,
                window: window.window_handle(),
                testing: true,
            }),
            channel: UpdateChannel::Stable,
            checking_enabled: false,
        });

        cancel_install(cx);

        assert_eq!(status(cx), Status::Available(release));
        assert!(cx.global::<AppUpdate>().pending.is_none());
        assert!(fs::read_dir(&install_root).unwrap().next().is_none());
    });
}

#[gpui::test]
fn continuing_applies_the_plan_before_reporting_relaunch_failure(cx: &mut TestAppContext) {
    let staging = scratch("continue-staging");
    let install_root = scratch("continue-install");
    fs::write(staging.join(install::SHELL_EXTENSION_DLL), "new dll").unwrap();
    let plan = install::plan(&staging, &install_root);
    let release = undated("v2.0.0");
    let window = cx.add_empty_window();

    window.update(|window, cx| {
        cx.set_global(AppUpdate {
            status: Status::AwaitingFileUse(release),
            testing: true,
            pending: Some(PendingInstall {
                release: undated("v2.0.0"),
                staged: staging.clone(),
                install: install_root.clone(),
                plan,
                window: window.window_handle(),
                testing: true,
            }),
            channel: UpdateChannel::Stable,
            checking_enabled: false,
        });

        continue_install(cx);

        assert_eq!(status(cx), Status::InstallFailed(InstallError::Relaunch));
        assert!(cx.global::<AppUpdate>().pending.is_none());
        assert_eq!(
            fs::read_to_string(install_root.join(install::SHELL_EXTENSION_DLL)).unwrap(),
            "new dll"
        );
    });
}
