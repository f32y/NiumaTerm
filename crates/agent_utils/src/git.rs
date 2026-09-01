//! Small git queries shared by the conversation pane and the git chrome.
//!
//! Both sides ask the repository the same questions from different views
//! (the pane's branch label, the sidebar's status), so the process-spawning
//! primitive and the branch query live here rather than growing a copy per
//! consumer.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use nmt_platform::windows::process::hidden_command;
use parking_lot::Mutex;

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
#[derive(Clone)]
pub enum CheckedOut {
    Branch(String),
    /// Short commit id of a detached `HEAD`, so the caller never presents an
    /// empty branch label.
    Detached(String),
}

/// One directory's branch as it was last read.
struct ReadBranch {
    at: Instant,
    /// `None` records that the directory is no repository, which is an answer
    /// worth holding: it is the case that runs git twice.
    answer: Option<CheckedOut>,
}

/// Answers already read, by working directory. Each conversation tab watches
/// the branch of its own directory and the title bar watches the repository
/// root, so several watchers ask git the same question about one directory and
/// each was opening a process for it.
static READ_BRANCHES: OnceLock<Mutex<HashMap<String, ReadBranch>>> = OnceLock::new();

/// How long an unvisited directory's answer is kept. Watchers poll in tens of
/// seconds, so anything this old belongs to a directory nothing is watching
/// any more and the map would otherwise grow for the life of the process.
const BRANCH_RETENTION: Duration = Duration::from_secs(600);

/// The checked-out branch for a working directory, or the short commit for a
/// detached `HEAD`. `None` when `dir` is no repository at all.
///
/// An answer read less than `max_age` ago is returned without running git, so
/// the watchers of one directory cost one process between them rather than one
/// each. Passing the caller's own polling interval keeps that to a single
/// process per directory per interval, and bounds how far a branch label can
/// lag a real switch at one further interval.
///
/// Runs git on the calling thread, so callers poll from a background one.
pub fn current_branch(cwd: &str, max_age: Duration) -> Option<CheckedOut> {
    let cache = READ_BRANCHES.get_or_init(Mutex::default);

    // Scoped so the lock is released before git runs: holding it across a
    // process spawn would serialize every other directory's watcher behind
    // this one.
    let fresh = {
        let entries = cache.lock();
        entries
            .get(cwd)
            .filter(|read| read.at.elapsed() < max_age)
            .map(|read| read.answer.clone())
    };

    if let Some(answer) = fresh {
        return answer;
    }

    let answer = read_current_branch(cwd);

    let mut entries = cache.lock();
    entries.retain(|_, read| read.at.elapsed() < BRANCH_RETENTION);
    entries.insert(
        cwd.to_string(),
        ReadBranch {
            at: Instant::now(),
            answer: answer.clone(),
        },
    );

    answer
}

/// A detached `HEAD` costs a second call: `rev-parse` resolves one revision at
/// a time, so no single invocation reports both the symbolic name and the short
/// commit to fall back on.
fn read_current_branch(cwd: &str) -> Option<CheckedOut> {
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
