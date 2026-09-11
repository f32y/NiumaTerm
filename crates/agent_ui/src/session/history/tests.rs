use std::time::SystemTime;

use nmt_agent::chat::{SessionScope, SessionSummary};

use crate::SessionHistoryUi;
use crate::session::history::CountPublication;

fn rows(id: &str) -> Vec<SessionSummary> {
    vec![SessionSummary {
        id: id.into(),
        title: id.into(),
        branch: None,
        cwd: Some("project".into()),
        last_active: SystemTime::UNIX_EPOCH,
        snippet: None,
    }]
}

#[test]
fn late_count_cannot_replace_new_scope_loading_or_completed_rows() {
    let mut history = SessionHistoryUi::default();
    let old = history.begin_filesystem_history(Some("project".into()), 1);

    history.data.scope = SessionScope::AllDirectories;

    let new = history.begin_filesystem_history(Some("project".into()), 1);

    assert!(matches!(
        history.publish_filesystem_count(&new, Some("project"), 1, 3),
        CountPublication::LoadRows
    ));
    assert!(matches!(
        history.publish_filesystem_count(&old, Some("project"), 1, 0),
        CountPublication::Stale
    ));
    assert_eq!(history.data.pending, Some(3));
    assert!(history.publish_filesystem_rows(&new, Some("project"), 1, rows("new")));
    assert!(matches!(
        history.publish_filesystem_count(&old, Some("project"), 1, 7),
        CountPublication::Stale
    ));
    assert_eq!(history.data.pending, None);
    assert_eq!(history.data.sessions, rows("new"));
}

#[test]
fn late_rows_cannot_replace_new_rows_after_scope_returns_to_original() {
    let mut history = SessionHistoryUi::default();
    let old = history.begin_filesystem_history(Some("project".into()), 1);

    assert!(matches!(
        history.publish_filesystem_count(&old, Some("project"), 1, 1),
        CountPublication::LoadRows
    ));

    history.data.scope = SessionScope::AllDirectories;

    let middle = history.begin_filesystem_history(Some("project".into()), 1);

    history.data.scope = SessionScope::CurrentDirectory;

    let new = history.begin_filesystem_history(Some("project".into()), 1);

    assert!(matches!(
        history.publish_filesystem_count(&new, Some("project"), 1, 1),
        CountPublication::LoadRows
    ));
    assert!(!history.publish_filesystem_rows(&old, Some("project"), 1, rows("old")));
    assert_eq!(history.data.pending, Some(1));
    assert!(history.publish_filesystem_rows(&new, Some("project"), 1, rows("new")));
    assert!(!history.publish_filesystem_rows(&old, Some("project"), 1, rows("old")));
    assert!(matches!(
        history.publish_filesystem_count(&middle, Some("project"), 1, 9),
        CountPublication::Stale
    ));
    assert_eq!(history.data.sessions, rows("new"));
    assert_eq!(history.data.pending, None);
}

#[test]
fn empty_count_finishes_loading_and_removes_previous_rows() {
    let mut history = SessionHistoryUi {
        selected: 8,
        ..Default::default()
    };

    history.data.sessions = rows("previous");

    let request = history.begin_filesystem_history(None, 1);

    assert!(matches!(
        history.publish_filesystem_count(&request, None, 1, 0),
        CountPublication::Empty
    ));
    assert!(history.data.sessions.is_empty());
    assert_eq!(history.selected, 0);
    assert_eq!(history.data.pending, None);
    assert!(!history.publish_filesystem_rows(&request, None, 1, rows("late")));
}

#[test]
fn replacement_invalidation_rejects_both_passes_and_clears_placeholders() {
    let mut history = SessionHistoryUi::default();
    let old = history.begin_filesystem_history(None, 1);

    assert!(matches!(
        history.publish_filesystem_count(&old, None, 1, 4),
        CountPublication::LoadRows
    ));

    history.invalidate_filesystem_history();

    assert_eq!(history.data.pending, None);
    assert!(matches!(
        history.publish_filesystem_count(&old, None, 1, 9),
        CountPublication::Stale
    ));
    assert!(!history.publish_filesystem_rows(&old, None, 1, rows("old")));

    let new = history.begin_filesystem_history(None, 2);

    assert!(matches!(
        history.publish_filesystem_count(&new, None, 2, 1),
        CountPublication::LoadRows
    ));
    assert!(history.publish_filesystem_rows(&new, None, 2, rows("replacement")));
    assert_eq!(history.data.sessions, rows("replacement"));
}

#[test]
fn changed_session_directory_or_scope_rejects_publication() {
    for (scope, cwd, epoch) in [
        (SessionScope::CurrentDirectory, Some("project"), 2),
        (SessionScope::CurrentDirectory, Some("other"), 1),
        (SessionScope::AllDirectories, Some("project"), 1),
    ] {
        let mut history = SessionHistoryUi::default();
        let request = history.begin_filesystem_history(Some("project".into()), 1);

        history.data.scope = scope;

        assert!(matches!(
            history.publish_filesystem_count(&request, cwd, epoch, 5),
            CountPublication::Stale
        ));
        assert!(!history.publish_filesystem_rows(&request, cwd, epoch, rows("wrong")));
        assert!(history.data.sessions.is_empty());
        assert_eq!(history.data.pending, None);
    }
}
