use std::path::PathBuf;
use std::{env, fs, process};

use crate::background_task::BackgroundTaskState;
use crate::chat::Item;
use crate::claude_code::shell_output::{MAX_OUTPUT_BYTES, shell_items};
use crate::claude_code::tasks::ShellDetail;

fn scratch_file(name: &str, contents: &[u8]) -> PathBuf {
    let dir = env::temp_dir().join(format!("nmt-shell-output-{}", process::id()));
    fs::create_dir_all(&dir).expect("scratch directory is writable");
    let path = dir.join(name);
    fs::write(&path, contents).expect("scratch file is writable");
    path
}

fn detail(output_file: Option<String>, state: BackgroundTaskState) -> ShellDetail {
    ShellDetail {
        id: "b8vo1ylgc".to_string(),
        command: Some("cargo build".to_string()),
        description: Some("Build the app".to_string()),
        output_file,
        state,
    }
}

fn command_item(detail: &ShellDetail) -> (String, Option<String>, Option<String>) {
    match shell_items(detail).remove(0) {
        Item::CommandExecution {
            command,
            aggregated_output,
            status,
            ..
        } => (command, aggregated_output, status),
        other => panic!("a shell renders as a command card, got {other:?}"),
    }
}

#[test]
fn a_running_command_reports_its_output_so_far() {
    let path = scratch_file("running.output", b"compiling\n");
    let detail = detail(
        Some(path.to_string_lossy().into_owned()),
        BackgroundTaskState::Working,
    );

    let (command, output, status) = command_item(&detail);

    assert_eq!(command, "cargo build");
    assert_eq!(output.as_deref(), Some("compiling\n"));
    assert_eq!(status.as_deref(), Some("inProgress"));
}

#[test]
fn a_settled_command_carries_the_status_its_row_reports() {
    let path = scratch_file("settled.output", b"error: build failed\n");
    let file = path.to_string_lossy().into_owned();

    let (_, _, status) = command_item(&detail(Some(file.clone()), BackgroundTaskState::Failed));
    assert_eq!(status.as_deref(), Some("failed"));

    let (_, _, status) = command_item(&detail(Some(file), BackgroundTaskState::Done));
    assert_eq!(status.as_deref(), Some("completed"));
}

#[test]
fn an_output_file_longer_than_the_bound_is_shown_from_its_end() {
    let overflow = MAX_OUTPUT_BYTES as usize + 1024;
    let mut contents = vec![b'a'; overflow];
    contents.extend_from_slice(b"final line\n");
    let path = scratch_file("long.output", &contents);

    let (_, output, _) = command_item(&detail(
        Some(path.to_string_lossy().into_owned()),
        BackgroundTaskState::Working,
    ));

    let output = output.expect("the tail is readable");
    assert!(output.ends_with("final line\n"));
    assert!(output.len() <= MAX_OUTPUT_BYTES as usize);
}

#[test]
fn a_command_with_nothing_written_yet_shows_no_output() {
    let path = scratch_file("empty.output", b"   \n");
    let file = path.to_string_lossy().into_owned();

    let (_, output, _) = command_item(&detail(Some(file), BackgroundTaskState::Working));
    assert_eq!(output, None);

    // A still-running command has no output file to name at all until its
    // completion notification reports one.
    let (_, output, _) = command_item(&detail(None, BackgroundTaskState::Working));
    assert_eq!(output, None);
}
