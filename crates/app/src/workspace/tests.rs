use nmt_terminal::event::{ProgressReport, ProgressState};

use crate::workspace::*;

/// Summaries with the given cwds, ids = 1-based position.
fn summaries(cwds: &[&str]) -> Vec<WorkspaceSummary> {
    multi_root_summaries(&cwds.iter().map(|cwd| vec![*cwd]).collect::<Vec<_>>())
}

/// Summaries owning the given directory lists, primary first, ids = 1-based
/// position.
fn multi_root_summaries(roots: &[Vec<&str>]) -> Vec<WorkspaceSummary> {
    roots
        .iter()
        .enumerate()
        .map(|(i, cwds)| WorkspaceSummary {
            id: WorkspaceId(i as u64 + 1),
            name: format!("Workspace {}", i + 1),
            cwd: cwds[0].to_string(),
            additional_cwds: cwds[1..].iter().map(|cwd| cwd.to_string()).collect(),
            active: i == 0,
            agent_status: AgentRuntimeStatus::Idle,
            terminal_activity: TerminalActivity::Idle,
            unread_count: 0,
            latest_unread_text: None,
            pinned: false,
            closeable: roots.len() > 1,
            temporary: false,
            kind: WorkspaceKind::Normal,
            progress: ProgressTally::default(),
        })
        .collect()
}

/// A manager holding `normal` normal workspaces (ids 1..=normal), with a
/// settings entry appended last when `settings` is set (id 100).
fn manager(normal: u64, settings: bool) -> WorkspaceManager {
    let tabs = || {
        TabManager::new(
            TabSurface::Pending(Box::default()),
            TabId(0),
            "Tab".to_string(),
        )
    };

    let mut manager = WorkspaceManager::new(
        tabs(),
        WorkspaceId(1),
        "Workspace 1".to_string(),
        WorkspaceRoots::single("C:/one".to_string()),
    );

    for id in 2..=normal {
        manager.new_workspace(
            tabs(),
            WorkspaceId(id),
            format!("Workspace {id}"),
            WorkspaceRoots::single(format!("C:/{id}")),
        );
    }

    if settings {
        manager.new_workspace_of_kind(
            TabManager::new(TabSurface::Settings, TabId(100), "Settings".to_string()),
            WorkspaceId(100),
            "Settings".to_string(),
            None,
            WorkspaceKind::Settings,
        );
    }

    manager
}

#[test]
fn workspace_progress_averages_the_tabs_reporting_a_percentage() {
    let mut tabs = TabManager::new(
        TabSurface::Pending(Box::default()),
        TabId(1),
        "Tab".to_string(),
    );
    tabs.new_tab(
        TabSurface::Pending(Box::default()),
        TabId(2),
        "Tab".to_string(),
    );
    tabs.new_tab(
        TabSurface::Pending(Box::default()),
        TabId(3),
        "Tab".to_string(),
    );

    assert_eq!(tabs_progress(&tabs).fraction(), None);

    let set = |progress| ProgressReport {
        state: ProgressState::Set,
        progress,
    };
    tabs.set_progress(TabId(1), set(Some(50)));
    tabs.set_progress(TabId(2), set(Some(100)));
    // No percentage to add, so this tab stays out of the average.
    tabs.set_progress(
        TabId(3),
        ProgressReport {
            state: ProgressState::Indeterminate,
            progress: None,
        },
    );

    assert_eq!(tabs_progress(&tabs).fraction(), Some(0.75));
}

#[test]
fn terminal_percentages_and_agent_tasks_share_one_scale() {
    // A half-finished command plus a task list with one of three items
    // done: 1.5 units of 4.
    let tally = ProgressTally::percent(50).merge(ProgressTally::tasks(1, 3));

    assert_eq!(tally.fraction(), Some(1.5 / 4.0));
    assert_eq!(ProgressTally::tasks(0, 0).fraction(), None);
}

#[test]
fn an_unseen_result_outranks_a_running_command() {
    let running = TerminalActivity::Running;
    let succeeded = TerminalActivity::Finished(CommandOutcome::Succeeded);
    let failed = TerminalActivity::Finished(CommandOutcome::Failed);

    assert_eq!(running.merge(succeeded), succeeded);
    assert_eq!(succeeded.merge(running), succeeded);
    assert_eq!(succeeded.merge(failed), failed);
    assert_eq!(failed.merge(succeeded), failed);
    assert_eq!(TerminalActivity::Idle.merge(running), running);
    assert_eq!(
        TerminalActivity::Idle.merge(TerminalActivity::Idle),
        TerminalActivity::Idle
    );
}

#[test]
fn settings_entry_is_left_out_of_the_real_count() {
    let manager = manager(1, true);

    assert_eq!(manager.len(), 2);
    assert_eq!(manager.real_len(), 1);
    assert_eq!(manager.settings_id(), Some(WorkspaceId(100)));
    assert_eq!(manager.kind_of(WorkspaceId(1)), Some(WorkspaceKind::Normal));
}

#[test]
fn a_lone_normal_workspace_stays_closed_off_beside_settings() {
    let summaries = manager(1, true).summaries();

    // The one real workspace routes into the quit/replace decision rather
    // than closing outright; the settings entry always closes.
    assert!(!summaries[0].closeable);
    assert!(summaries[1].closeable);
}

#[test]
fn settings_closes_without_taking_the_last_slot() {
    let mut manager = manager(1, true);

    assert!(manager.close_workspace(WorkspaceId(100)).is_some());
    assert_eq!(manager.len(), 1);
    assert_eq!(manager.settings_id(), None);
    assert_eq!(manager.active_id(), WorkspaceId(1));
}

#[test]
fn settings_reorders_like_any_other_entry() {
    let mut manager = manager(2, true);

    manager.reorder(2, 0);

    let order: Vec<_> = manager.summaries().into_iter().map(|ws| ws.id).collect();

    assert_eq!(
        order,
        vec![WorkspaceId(100), WorkspaceId(1), WorkspaceId(2)]
    );
    // The settings entry was active before the move and stays active.
    assert_eq!(manager.active_id(), WorkspaceId(100));
}

#[test]
fn leaving_settings_lands_on_a_normal_workspace() {
    let mut manager = manager(2, true);

    assert_eq!(manager.active_kind(), WorkspaceKind::Settings);

    manager.activate(manager.first_normal_index());

    assert_eq!(manager.active_id(), WorkspaceId(1));
    assert_eq!(manager.active_kind(), WorkspaceKind::Normal);
}

fn matched(cwds: &[&str], target: &str) -> Option<WorkspaceId> {
    best_match(&summaries(cwds), path::Path::new(target))
}

fn exactly_matched(cwds: &[&str], target: &str) -> Option<WorkspaceId> {
    exact_match(&summaries(cwds), path::Path::new(target))
}

#[test]
fn deepest_ancestor_wins() {
    assert_eq!(
        matched(&["C:/A/B", "C:/A"], "C:/A/B/C"),
        Some(WorkspaceId(1))
    );
}

#[test]
fn shallow_ancestor_matches_when_deep_does_not() {
    assert_eq!(matched(&["C:/A/B", "C:/A"], "C:/A/D"), Some(WorkspaceId(2)));
}

#[test]
fn unrelated_target_matches_nothing() {
    assert_eq!(matched(&["C:/A/B", "C:/A"], "C:/E"), None);
}

#[test]
fn equal_path_matches() {
    assert_eq!(matched(&["C:/A/B"], "C:/A/B"), Some(WorkspaceId(1)));
}

#[test]
fn match_is_case_insensitive_and_separator_agnostic() {
    assert_eq!(matched(&["c:\\a\\b\\"], "C:/A/B/C"), Some(WorkspaceId(1)));
}

#[test]
fn component_boundary_is_respected() {
    assert_eq!(matched(&["C:/A/B"], "C:/A/BC"), None);
}

#[test]
fn placeholder_cwds_are_skipped() {
    assert_eq!(matched(&[".", "", "  "], "C:/A"), None);
}

#[test]
fn tie_on_depth_goes_to_the_earlier_workspace() {
    assert_eq!(matched(&["C:/A", "c:/a"], "C:/A/B"), Some(WorkspaceId(1)));
}

#[test]
fn exact_match_reuses_the_same_workspace_path() {
    assert_eq!(
        exactly_matched(&["C:/A", "c:\\work\\project\\"], "C:/WORK/PROJECT"),
        Some(WorkspaceId(2))
    );
}

#[test]
fn exact_match_does_not_reuse_ancestor_workspace() {
    assert_eq!(exactly_matched(&["C:/A"], "C:/A/child"), None);
}

/// Best match over workspaces that own several directories each.
fn multi_root_matched(roots: &[Vec<&str>], target: &str) -> Option<WorkspaceId> {
    best_match(&multi_root_summaries(roots), path::Path::new(target))
}

/// Exact match over workspaces that own several directories each.
fn multi_root_exactly_matched(roots: &[Vec<&str>], target: &str) -> Option<WorkspaceId> {
    exact_match(&multi_root_summaries(roots), path::Path::new(target))
}

#[test]
fn an_additional_directory_makes_its_workspace_eligible() {
    assert_eq!(
        multi_root_matched(&[vec!["C:/X"], vec!["C:/A", "C:/B"]], "C:/B/inner"),
        Some(WorkspaceId(2))
    );
    assert_eq!(
        multi_root_exactly_matched(&[vec!["C:/X"], vec!["C:/A", "C:/B"]], r"c:\b"),
        Some(WorkspaceId(2))
    );
}

#[test]
fn the_longest_matching_root_wins_across_workspaces() {
    assert_eq!(
        multi_root_matched(&[vec!["C:/A", "C:/A/B/C"], vec!["C:/A/B"]], "C:/A/B/C/d"),
        Some(WorkspaceId(1))
    );
}

#[test]
fn a_primary_directory_outranks_an_equal_additional_directory() {
    assert_eq!(
        multi_root_matched(&[vec!["C:/X", "C:/A"], vec!["C:/A", "C:/Y"]], "C:/A/file"),
        Some(WorkspaceId(2))
    );
    assert_eq!(
        multi_root_exactly_matched(&[vec!["C:/X", "C:/A"], vec!["C:/A"]], "C:/A"),
        Some(WorkspaceId(2))
    );
}

#[test]
fn equal_roots_of_the_same_rank_go_to_the_earlier_workspace() {
    assert_eq!(
        multi_root_matched(&[vec!["C:/X", "C:/A"], vec!["C:/Y", "c:/a"]], "C:/A/file"),
        Some(WorkspaceId(1))
    );
}

#[test]
fn placeholder_additional_directories_are_skipped() {
    assert_eq!(multi_root_matched(&[vec!["C:/X", ".", "  "]], "C:/A"), None);
}

/// The ordered directories of `roots`, primary first.
fn ordered(roots: &WorkspaceRoots) -> Vec<&str> {
    roots.ordered().collect()
}

#[test]
fn additional_directories_keep_the_order_they_were_added_in() {
    let roots = WorkspaceRoots::new(
        "C:/A".into(),
        vec!["C:/B".into(), "C:/C".into(), "C:/D".into()],
    );
    assert_eq!(ordered(&roots), ["C:/A", "C:/B", "C:/C", "C:/D"]);
    assert_eq!(roots.primary(), "C:/A");
}

#[test]
fn an_equivalent_path_spelling_is_rejected_as_a_duplicate() {
    let mut roots = WorkspaceRoots::single("C:/Work/Project".into());
    assert_eq!(
        roots.add(r"c:\work\project\\".into()),
        RootChange::Duplicate
    );
    assert_eq!(ordered(&roots), ["C:/Work/Project"]);
    assert_eq!(roots.add("C:/Work/Project/.".into()), RootChange::Duplicate);
    assert_eq!(ordered(&roots), ["C:/Work/Project"]);
}

#[test]
fn a_repeated_entry_in_a_saved_list_is_dropped_once() {
    let roots = WorkspaceRoots::new(
        "C:/A".into(),
        vec!["C:/B".into(), r"c:\a".into(), "C:/B/".into()],
    );
    assert_eq!(ordered(&roots), ["C:/A", "C:/B"]);
}

#[test]
fn a_nested_directory_is_owned_alongside_its_ancestor() {
    let mut roots = WorkspaceRoots::single("C:/A".into());
    assert_eq!(roots.add("C:/A/child".into()), RootChange::Applied);
    assert_eq!(ordered(&roots), ["C:/A", "C:/A/child"]);
}

#[test]
fn making_a_directory_primary_preserves_every_other_position() {
    let mut roots = WorkspaceRoots::new(
        "C:/A".into(),
        vec!["C:/B".into(), "C:/C".into(), "C:/D".into()],
    );
    assert_eq!(roots.make_primary(r"c:\c"), RootChange::Applied);
    assert_eq!(ordered(&roots), ["C:/C", "C:/A", "C:/B", "C:/D"]);
}

#[test]
fn making_the_current_primary_primary_again_changes_nothing() {
    let mut roots = WorkspaceRoots::new("C:/A".into(), vec!["C:/B".into()]);
    assert_eq!(roots.make_primary("C:/A"), RootChange::Applied);
    assert_eq!(ordered(&roots), ["C:/A", "C:/B"]);
}

#[test]
fn an_unattached_directory_cannot_be_promoted_or_removed() {
    let mut roots = WorkspaceRoots::new("C:/A".into(), vec!["C:/B".into()]);
    assert_eq!(roots.make_primary("C:/Z"), RootChange::NotAttached);
    assert_eq!(roots.remove("C:/Z"), RootChange::NotAttached);
    assert_eq!(ordered(&roots), ["C:/A", "C:/B"]);
}

#[test]
fn removing_the_primary_promotes_the_first_additional_directory() {
    let mut roots = WorkspaceRoots::new("C:/A".into(), vec!["C:/B".into(), "C:/C".into()]);
    assert_eq!(roots.remove("C:/A"), RootChange::Applied);
    assert_eq!(ordered(&roots), ["C:/B", "C:/C"]);
}

#[test]
fn the_last_directory_of_a_normal_workspace_cannot_be_removed() {
    let mut roots = WorkspaceRoots::single("C:/A".into());
    assert_eq!(roots.remove("C:/A"), RootChange::WouldBeEmpty);
    assert_eq!(ordered(&roots), ["C:/A"]);
}

#[test]
fn additional_directories_do_not_displace_the_primary_default() {
    let mut manager = manager(1, false);
    let id = manager.active_id();

    manager.set_roots(
        id,
        WorkspaceRoots::new("C:/one".into(), vec!["C:/two".into(), "C:/three".into()]),
    );

    // New terminals, new Agent Tabs, generated labels, relative link
    // resolution, and Git discovery all read this one accessor.
    assert_eq!(manager.active_cwd(), "C:/one");
    assert_eq!(
        manager.active_roots().map(ordered),
        Some(vec!["C:/one", "C:/two", "C:/three"])
    );

    let summary = manager.summaries().remove(0);
    assert_eq!(summary.cwd, "C:/one");
    assert_eq!(summary.additional_cwds, ["C:/two", "C:/three"]);

    // Promotion is what moves the defaults; attaching a directory does not.
    let mut promoted = manager.roots_of(id).expect("roots").clone();
    assert_eq!(promoted.make_primary("C:/three"), RootChange::Applied);
    manager.set_roots(id, promoted);
    assert_eq!(manager.active_cwd(), "C:/three");
}

#[test]
fn the_location_free_settings_entry_owns_no_directory() {
    let mut manager = manager(1, true);
    let settings = manager.settings_id().expect("settings entry");

    assert_eq!(manager.roots_of(settings), None);
    // The Settings entry stays location-free even when a caller offers it one.
    manager.set_roots(settings, WorkspaceRoots::single("C:/two".into()));
    assert_eq!(manager.roots_of(settings), None);

    let summary = manager
        .summaries()
        .into_iter()
        .find(|ws| ws.id == settings)
        .expect("settings summary");
    assert!(summary.cwd.is_empty());
    assert!(summary.additional_cwds.is_empty());
}

#[test]
fn workspace_identity_survives_root_edits() {
    let mut manager = manager(3, false);
    let second = WorkspaceId(2);

    manager.set_temporary(second, true);
    manager.set_roots(
        second,
        WorkspaceRoots::new("C:/2".into(), vec!["C:/extra".into()]),
    );

    // Adoption, pin state, order, and closeability all key on the workspace
    // id, so attaching a directory leaves every one of them untouched.
    manager.set_temporary(second, false);
    manager.set_pinned(second, true);
    assert!(manager.is_pinned(second));
    assert_eq!(
        manager.summaries().first().map(|ws| ws.id),
        Some(second),
        "pinning moves the workspace into the pinned group"
    );
    assert_eq!(
        manager.roots_of(second).map(ordered),
        Some(vec!["C:/2", "C:/extra"])
    );

    // A pinned workspace refuses to close; unpinning restores that.
    assert!(manager.close_workspace(second).is_none());
    manager.set_pinned(second, false);

    manager.reorder(0, 2);
    let ids: Vec<_> = manager.summaries().iter().map(|ws| ws.id).collect();
    assert_eq!(ids, [WorkspaceId(1), WorkspaceId(3), second]);

    let closed = manager.close_workspace(second).expect("closeable");
    assert_eq!(closed.id(), second);

    // The detached directory leaves with its workspace and stops routing.
    assert_eq!(
        best_match(&manager.summaries(), path::Path::new("C:/extra/file")),
        None
    );
}
