use std::fs;

use gpui::TestAppContext;
use nmt_agent::AgentWorkspace;
use nmt_agent::team::model::UserInput;
use nmt_agent::team::session::{TeamError, TeamSession};
use tempfile::tempdir;

use crate::agent_tab::team::{TeamCommand, TeamRuntime};

#[gpui::test]
async fn queued_changes_finish_before_close_even_after_the_caller_drops_its_reply(
    cx: &mut TestAppContext,
) {
    let directory = tempdir().unwrap();

    let (runtime, id, closed) = cx.update(|cx| {
        let runtime = TeamRuntime::create(directory.path(), AgentWorkspace::default(), cx);
        let id = runtime.read(cx).id();

        let closed = runtime.update(cx, |runtime, cx| {
            for text in ["one", "two", "three"] {
                drop(runtime.command(TeamCommand::Correction(correction(text)), cx));
            }

            assert!(runtime.room().messages().is_empty());

            runtime.close(cx)
        });

        (runtime, id, closed)
    });

    let weak = runtime.downgrade();

    drop(runtime);

    closed.await.unwrap();
    cx.update(|_| {});
    cx.run_until_parked();

    assert!(weak.upgrade().is_none());

    let reopened = TeamSession::open(directory.path(), id).unwrap();

    assert_eq!(reopened.store().room().messages().len(), 3);
}

fn correction(text: &str) -> UserInput {
    UserInput {
        text: text.to_string(),
        references: Vec::new(),
    }
}

#[gpui::test]
async fn failed_initialization_resolves_queued_and_later_commands(cx: &mut TestAppContext) {
    let directory = tempdir().unwrap();
    let blocked = directory.path().join("file");

    fs::write(&blocked, "not a directory").unwrap();

    let (runtime, command) = cx.update(|cx| {
        let runtime = TeamRuntime::create(&blocked, AgentWorkspace::default(), cx);

        let command = runtime.update(cx, |runtime, cx| {
            runtime.command(TeamCommand::Correction(correction("one")), cx)
        });

        (runtime, command)
    });

    assert!(matches!(command.await, Err(TeamError::Unavailable)));

    let command = runtime.update(cx, |runtime, cx| {
        assert!(!runtime.loading());
        assert!(runtime.error().is_some());

        runtime.command(TeamCommand::Correction(correction("two")), cx)
    });

    assert!(matches!(command.await, Err(TeamError::Unavailable)));
}
