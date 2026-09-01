//! Several watchers ask for the branch of one directory, so the answer is held
//! between reads. What that must never do is pin an answer a caller asked to be
//! fresh, which is what would leave a tab naming the branch the user just left.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::time::Duration;
use std::{env, fs};

use nmt_agent_utils::git::{CheckedOut, current_branch};

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("run git");

    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

fn repository() -> PathBuf {
    let dir = env::temp_dir().join(format!("nmt-git-branch-reads-{}", process::id()));

    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).expect("create test repository directory");

    git(&dir, &["init", "--initial-branch=trunk"]);
    git(&dir, &["config", "user.email", "test@example.invalid"]);
    git(&dir, &["config", "user.name", "Branch Read Test"]);
    git(&dir, &["commit", "--allow-empty", "-m", "root"]);

    dir
}

fn branch_of(dir: &Path, max_age: Duration) -> Option<String> {
    match current_branch(&dir.to_string_lossy(), max_age)? {
        CheckedOut::Branch(branch) => Some(branch),
        CheckedOut::Detached(commit) => Some(commit),
    }
}

#[test]
fn a_held_answer_is_shared_but_never_outlives_the_freshness_asked_for() {
    let dir = repository();

    assert_eq!(branch_of(&dir, Duration::ZERO).as_deref(), Some("trunk"));

    git(&dir, &["checkout", "-b", "topic"]);

    // A watcher that accepts an answer this recent gets the held one, which is
    // what keeps every tab on this directory to one git process between them.
    assert_eq!(
        branch_of(&dir, Duration::from_secs(600)).as_deref(),
        Some("trunk"),
        "a caller accepting a recent answer reuses the one already read"
    );

    // A watcher that accepts nothing older than its own poll reads again, so
    // the label follows the switch rather than being pinned by the hold.
    assert_eq!(
        branch_of(&dir, Duration::ZERO).as_deref(),
        Some("topic"),
        "a caller asking for a fresh answer must not be served the held one"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_directory_outside_a_repository_has_no_branch() {
    let dir = env::temp_dir().join(format!("nmt-git-branch-bare-{}", process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).expect("create non-repository directory");

    assert!(branch_of(&dir, Duration::ZERO).is_none());

    fs::remove_dir_all(&dir).ok();
}
