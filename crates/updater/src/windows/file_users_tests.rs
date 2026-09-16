use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::slice;
use std::sync::Arc;

use nmt_platform::windows::restart_manager::{
    AffectedApplication, ApplicationKind, FileUsage, Operation, RebootReasons, RestartManagerError,
};
use parking_lot::Mutex;
use tempfile::tempdir;

use crate::windows::file_users::{
    ApplyOutcome, ClosePreparation, ClosedFileUsers, FileUsePromptReason, FileUserSession,
    classify_file_usage, prepare_close_with, recovery_application_names,
};
use crate::windows::install::{InstallError, Installation, SHELL_EXTENSION_DLL};
use crate::windows::releases::Release;

thread_local! {
    static NEXT_SESSION: RefCell<Option<Arc<Mutex<CloseState>>>> = const { RefCell::new(None) };
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

impl FileUserSession for ScriptedSession {
    fn open(_path: &Path) -> Result<Self, RestartManagerError> {
        let state = NEXT_SESSION.with_borrow_mut(|next| next.take().expect("configured session"));

        state.lock().events.push("open");

        Ok(Self { state })
    }

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

fn application(name: &str, process_id: u32, restartable: bool) -> AffectedApplication {
    AffectedApplication {
        name: name.to_owned(),
        service_name: None,
        process_id,
        kind: ApplicationKind::Explorer,
        terminal_session_id: Some(1),
        restartable,
    }
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
fn close_preparation_uses_a_fresh_session_application_list() {
    let current = application("Current host", 22, true);

    let state = Arc::new(Mutex::new(CloseState {
        usage: [Ok(FileUsage {
            applications: vec![current.clone()],
            reboot_reasons: RebootReasons::default(),
        })]
        .into(),
        shutdown_error: None,
        restart_error: None,
        events: Vec::new(),
    }));

    NEXT_SESSION.with_borrow_mut(|next| *next = Some(state.clone()));

    let result = prepare_close_with::<ScriptedSession>(Path::new(r"C:\NiumaTerm\dll"));

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
        usage: [
            Ok(FileUsage {
                applications: vec![current.clone()],
                reboot_reasons: RebootReasons::default(),
            }),
            Ok(FileUsage {
                applications: vec![current.clone()],
                reboot_reasons: RebootReasons::default(),
            }),
        ]
        .into(),
        shutdown_error: Some(351),
        restart_error: None,
        events: Vec::new(),
    }));

    NEXT_SESSION.with_borrow_mut(|next| *next = Some(state.clone()));

    let result = prepare_close_with::<ScriptedSession>(Path::new(r"C:\NiumaTerm\dll"));

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

#[test]
fn a_failed_file_replacement_still_restarts_closed_applications() {
    let staging = tempdir().unwrap();
    let install = tempdir().unwrap();
    let staged_file = staging.path().join(SHELL_EXTENSION_DLL);

    fs::write(&staged_file, "new dll").unwrap();

    let installation = Installation::new(
        Release {
            label: "v2.0.0".into(),
            page_url: String::new(),
            assets: Vec::new(),
            published: None,
        },
        staging.path().to_path_buf(),
        install.path().to_path_buf(),
        true,
    );

    // The captured plan names this file, so losing it before replacement
    // forces the copy to fail after the applications have already closed.
    fs::remove_file(staged_file).unwrap();

    let state = Arc::new(Mutex::new(CloseState {
        usage: VecDeque::new(),
        shutdown_error: None,
        restart_error: None,
        events: Vec::new(),
    }));

    let closed = ClosedFileUsers(ClosePreparation::Released {
        session: Box::new(ScriptedSession {
            state: state.clone(),
        }),
        applications: vec![application("Explorer", 11, true)],
    });

    assert!(matches!(
        closed.apply(&installation),
        ApplyOutcome::Failed(InstallError::NotWritable)
    ));
    assert_eq!(state.lock().events, ["restart"]);
}
