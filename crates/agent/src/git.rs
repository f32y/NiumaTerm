//! Small git queries shared by the conversation pane and the git chrome.
//!
//! Both sides ask the repository the same questions from different views
//! (the pane's branch label, the sidebar's status), so the process-spawning
//! primitive and the branch query live here rather than growing a copy per
//! consumer.

use std::collections::HashMap;

use std::process::Output;

use std::sync::OnceLock;

use std::time::{Duration, Instant};

use nmt_platform::process::hidden_command;

use parking_lot::Mutex;

use tracing::warn;

/// Run one git command in `dir` and return its stdout, with stderr folded
/// into the error text so a failed call explains itself.
pub fn run_git(dir: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = git_output(dir, args)?;

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

fn git_output(dir: &str, args: &[&str]) -> Result<Output, String> {
    hidden_command("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .map_err(|error| format!("failed to run git: {error}"))
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

    /// Cache a missing repository separately from a failed Git invocation.
    answer: Result<Option<CheckedOut>, String>,
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
/// detached `HEAD`. `Ok(None)` means the directory is not a repository;
/// command failures remain errors.
///
/// An answer read less than `max_age` ago is returned without running git, so
/// the watchers of one directory cost one process between them rather than one
/// each. Passing the caller's own polling interval keeps that to a single
/// process per directory per interval, and bounds how far a branch label can
/// lag a real switch at one further interval.
///
/// Runs git on the calling thread, so callers poll from a background one.
pub fn current_branch(cwd: &str, max_age: Duration) -> Result<Option<CheckedOut>, String> {
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

    if let Err(error) = &answer
        && entries
            .get(cwd)
            .and_then(|entry| entry.answer.as_ref().err())
            != Some(error)
    {
        warn!("failed to read git branch in {cwd}: {error}");
    }

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
fn read_current_branch(cwd: &str) -> Result<Option<CheckedOut>, String> {
    let output = git_output(cwd, &["branch", "--show-current"])?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();

        if error.contains("not a git repository") {
            return Ok(None);
        }

        return Err(format!("git branch exited with {}: {error}", output.status));
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if !branch.is_empty() {
        return Ok(Some(CheckedOut::Branch(branch)));
    }

    let commit = run_git(cwd, &["rev-parse", "--short", "HEAD"])?;
    let commit = String::from_utf8_lossy(&commit).trim().to_string();

    Ok((!commit.is_empty()).then_some(CheckedOut::Detached(commit)))
}
