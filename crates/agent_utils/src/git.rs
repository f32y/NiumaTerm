//! Small git queries shared by the conversation pane and the git chrome.
//!
//! Both sides ask the repository the same questions from different views
//! (the pane's branch label, the sidebar's status), so the process-spawning
//! primitive and the branch query live here rather than growing a copy per
//! consumer.

use nmt_platform::windows::process::hidden_command;

/// Run one git command in `dir` and return its stdout, with stderr folded
/// into the error text so a failed call explains itself.
pub fn run_git(dir: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = hidden_command("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|err| format!("failed to run git: {err}"))?;

    if !output.status.success() {
        return Err(format!(
            "git {} exited with {}: {}",
            args.first().unwrap_or(&""),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(output.stdout)
}

/// What `HEAD` points at. Presentation of the detached form is the caller's:
/// the label is localized, and this crate carries no catalog.
pub enum CheckedOut {
    Branch(String),
    /// Short commit id of a detached `HEAD`, so the caller never presents an
    /// empty branch label.
    Detached(String),
}

/// The checked-out branch for a working directory, or the short commit for a
/// detached `HEAD`. `None` when `dir` is no repository at all.
pub fn current_branch(cwd: &str) -> Option<CheckedOut> {
    let branch = run_git(cwd, &["branch", "--show-current"])
        .ok()
        .map(|out| String::from_utf8_lossy(&out).trim().to_string())
        .filter(|branch| !branch.is_empty());

    if let Some(branch) = branch {
        return Some(CheckedOut::Branch(branch));
    }

    let commit = run_git(cwd, &["rev-parse", "--short", "HEAD"]).ok()?;
    let commit = String::from_utf8_lossy(&commit).trim().to_string();
    (!commit.is_empty()).then_some(CheckedOut::Detached(commit))
}
