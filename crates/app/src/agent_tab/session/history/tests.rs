use std::time::SystemTime;

use nmt_agent::chat::{SessionScope, SessionSummary};

use crate::agent_tab::SessionHistoryUi;
use crate::agent_tab::session::history::CountPublication;
use crate::agent_tab::view::recent_sessions::RecentSessionsMode;

fn rows(id: &str) -> Vec<SessionSummary> {
    vec![SessionSummary {
        id: id.into(),
        title: id.into(),
        branch: None,
        cwd: Some("project".into()),
        last_active: SystemTime::UNIX_EPOCH,
        snippet: None,
        origin: None,
    }]
}

#[test]
fn late_count_cannot_replace_new_scope_loading_or_completed_rows() {
    let mut history = SessionHistoryUi::default();

    let old = history
        .data
        .begin_filesystem_history(Some("project".into()));

    history.data.scope = SessionScope::AllDirectories;

    let new = history
        .data
        .begin_filesystem_history(Some("project".into()));

    assert!(matches!(
        history.publish_filesystem_count(&new, Some("project"), 3),
        CountPublication::LoadRows
    ));
    assert!(matches!(
        history.publish_filesystem_count(&old, Some("project"), 0),
        CountPublication::Stale
    ));
    assert_eq!(history.data.pending, Some(3));
    assert!(history.publish_filesystem_rows(&new, Some("project"), rows("new")));
    assert!(matches!(
        history.publish_filesystem_count(&old, Some("project"), 7),
        CountPublication::Stale
    ));
    assert_eq!(history.data.pending, None);
    assert_eq!(history.data.sessions, rows("new"));
}

#[test]
fn late_rows_cannot_replace_new_rows_after_scope_returns_to_original() {
    let mut history = SessionHistoryUi::default();

    let old = history
        .data
        .begin_filesystem_history(Some("project".into()));

    assert!(matches!(
        history.publish_filesystem_count(&old, Some("project"), 1),
        CountPublication::LoadRows
    ));

    history.data.scope = SessionScope::AllDirectories;

    let middle = history
        .data
        .begin_filesystem_history(Some("project".into()));

    history.data.scope = SessionScope::CurrentDirectory;

    let new = history
        .data
        .begin_filesystem_history(Some("project".into()));

    assert!(matches!(
        history.publish_filesystem_count(&new, Some("project"), 1),
        CountPublication::LoadRows
    ));
    assert!(!history.publish_filesystem_rows(&old, Some("project"), rows("old")));
    assert_eq!(history.data.pending, Some(1));
    assert!(history.publish_filesystem_rows(&new, Some("project"), rows("new")));
    assert!(!history.publish_filesystem_rows(&old, Some("project"), rows("old")));
    assert!(matches!(
        history.publish_filesystem_count(&middle, Some("project"), 9),
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

    let request = history.data.begin_filesystem_history(None);

    assert!(matches!(
        history.publish_filesystem_count(&request, None, 0),
        CountPublication::Empty
    ));
    assert!(history.data.sessions.is_empty());
    assert_eq!(history.selected, 0);
    assert_eq!(history.data.pending, None);
    assert!(!history.publish_filesystem_rows(&request, None, rows("late")));
}

#[test]
fn replacement_invalidation_rejects_both_passes_and_clears_placeholders() {
    let mut history = SessionHistoryUi::default();

    let old = history.data.begin_filesystem_history(None);

    assert!(matches!(
        history.publish_filesystem_count(&old, None, 4),
        CountPublication::LoadRows
    ));

    history.data.invalidate_filesystem_history();

    assert_eq!(history.data.pending, None);
    assert!(matches!(
        history.publish_filesystem_count(&old, None, 9),
        CountPublication::Stale
    ));
    assert!(!history.publish_filesystem_rows(&old, None, rows("old")));

    let new = history.data.begin_filesystem_history(None);

    assert!(matches!(
        history.publish_filesystem_count(&new, None, 1),
        CountPublication::LoadRows
    ));
    assert!(history.publish_filesystem_rows(&new, None, rows("replacement")));
    assert_eq!(history.data.sessions, rows("replacement"));
}

#[test]
fn changed_directory_or_scope_rejects_publication() {
    for (scope, cwd) in [
        (SessionScope::CurrentDirectory, Some("other")),
        (SessionScope::AllDirectories, Some("project")),
    ] {
        let mut history = SessionHistoryUi::default();

        let request = history
            .data
            .begin_filesystem_history(Some("project".into()));

        history.data.scope = scope;

        assert!(matches!(
            history.publish_filesystem_count(&request, cwd, 5),
            CountPublication::Stale
        ));
        assert!(!history.publish_filesystem_rows(&request, cwd, rows("wrong")));
        assert!(history.data.sessions.is_empty());
        assert_eq!(history.data.pending, None);
    }
}

#[test]
fn a_resume_before_any_rows_waits_for_its_listing() {
    let mut history = SessionHistoryUi::default();

    // No rows and no listing started: the folder holds nothing to resume, so
    // there is nothing to wait for.
    assert!(!history.open());
    assert_eq!(history.mode, RecentSessionsMode::Hidden);

    let request = history
        .data
        .begin_filesystem_history(Some("project".into()));

    // The same request, still on its way. `/resume` waits for the rows
    // instead of reporting the folder empty.
    assert!(history.data.list_in_flight());
    assert!(!history.open());

    history.await_listing();

    assert_eq!(history.mode, RecentSessionsMode::AwaitingRows);
    assert!(
        !history.is_visible(false, true),
        "no rows are on screen yet"
    );

    // The rows arrive: the wait opens the list it asked for.
    assert!(matches!(
        history.publish_filesystem_count(&request, Some("project"), 1),
        CountPublication::LoadRows
    ));
    assert!(history.publish_filesystem_rows(&request, Some("project"), rows("project")));
    assert!(history.resolve_awaiting_listing(history.data.sessions.len()));
    assert_eq!(history.mode, RecentSessionsMode::Open);
    assert!(history.is_visible(false, true));
}

#[test]
fn a_listing_that_completes_with_no_rows_ends_the_wait_hidden() {
    let mut history = SessionHistoryUi::default();

    history.await_listing();

    assert!(!history.resolve_awaiting_listing(0));
    assert_eq!(history.mode, RecentSessionsMode::Hidden);
    assert!(!history.data.is_awaiting_rows());
}

#[test]
fn an_expired_wait_stops_blocking_the_list() {
    let mut history = SessionHistoryUi::default();

    history.await_listing();

    assert!(history.abandon_awaiting_listing());
    assert_eq!(history.mode, RecentSessionsMode::Hidden);
    assert!(!history.abandon_awaiting_listing(), "already ended");
}

#[test]
fn showing_another_surface_ends_a_wait_for_rows() {
    for mode in [
        RecentSessionsMode::Hidden,
        RecentSessionsMode::Open,
        RecentSessionsMode::Loading,
        RecentSessionsMode::Automatic,
    ] {
        let mut history = SessionHistoryUi::default();

        history.await_listing();

        history.show(mode);

        assert!(!history.data.is_awaiting_rows(), "{mode:?} keeps the wait");
        assert!(!history.resolve_awaiting_listing(1));
    }
}
