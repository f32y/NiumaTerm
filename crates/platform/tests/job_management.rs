//! Job-management integration: with the toggle on, the spawned shell must be
//! assigned to the PTY's kill-on-close Job Object.

#![cfg(windows)]

use nmt_platform::{create_pty, job_other_process_count, set_job_management};
use windows_sys::Win32::System::JobObjects::IsProcessInJob;

// One test fn: the toggle is process-global, so parallel test threads would
// race on it.
#[test]
fn job_management_toggle_controls_shell_job() {
    // Off (default): no job.
    let pty = create_pty("cmd.exe", Vec::new(), &None, 80, 24).expect("failed to create ConPTY");
    assert!(pty.job_handle().is_none());
    drop(pty);

    // On: the shell is inside the PTY's job.
    set_job_management(true);
    let pty = create_pty("cmd.exe", Vec::new(), &None, 80, 24).expect("failed to create ConPTY");
    set_job_management(false);

    let job = pty.job_handle().expect("job handle present when enabled");
    let mut in_job = 0;
    let ok = unsafe { IsProcessInJob(pty.child_watcher().raw_handle(), job, &mut in_job) };
    assert_ne!(ok, 0, "IsProcessInJob failed");
    assert_ne!(in_job, 0, "shell process is not in the PTY's job");
    // A fresh shell has no descendants: the job holds exactly one process.
    assert_eq!(job_other_process_count(job as isize), 0);
    // Dropping the Pty closes the job (KILL_ON_JOB_CLOSE reaps the tree).
}
