use nmt_platform::windows::restart_manager::{AffectedApplication, ApplicationKind};

use crate::update::file_users::display_names;

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
