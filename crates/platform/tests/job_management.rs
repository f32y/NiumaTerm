//! Job-management integration: with the toggle on, the spawned shell must be
//! assigned to the PTY's kill-on-close Job Object.

#![cfg(windows)]

use nmt_platform::{create_managed_pty_with_env, create_pty};

#[test]
fn managed_pty_controls_shell_process_tree() {
    let pty = create_pty("cmd.exe", Vec::new(), &None, 80, 24).expect("failed to create ConPTY");
    assert!(pty.process_tree().is_none());
    drop(pty);

    let pty = create_managed_pty_with_env(
        "cmd.exe",
        Vec::new(),
        &None,
        80,
        24,
        &[],
        Some("managed test"),
    )
    .expect("failed to create managed ConPTY");
    assert!(
        pty.process_tree().is_some(),
        "managed PTY must own its process tree"
    );
    let process_tree = pty.process_tree().expect("managed process tree");
    assert_eq!(process_tree.process_count(), 1);
    assert_eq!(process_tree.other_process_count(), 0);
}
