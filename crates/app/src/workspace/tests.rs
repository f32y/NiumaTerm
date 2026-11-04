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
            pinned: false,
            closeable: roots.len() > 1,
            temporary: false,
            tab_fold: TabFold::All,
            kind: WorkspaceKind::Normal,
            terminal_progress: ProgressTally::default(),
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
        manager.new_workspace_of_kind(
            tabs(),
            WorkspaceId(id),
            format!("Workspace {id}"),
            Some(WorkspaceRoots::single(format!("C:/{id}"))),
            WorkspaceKind::Normal,
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

fn matched(cwds: &[&str], target: &str) -> Option<WorkspaceId> {
    best_match(&summaries(cwds), path::Path::new(target))
}

#[cfg(windows)]
fn exactly_matched(cwds: &[&str], target: &str) -> Option<WorkspaceId> {
    exact_match(&summaries(cwds), path::Path::new(target))
}

/// Case folding and backslash separators are equivalences only the Windows
/// path rules grant; elsewhere these name different directories.
#[cfg(windows)]
#[test]
fn match_is_case_insensitive_and_separator_agnostic() {
    assert_eq!(matched(&["c:\\a\\b\\"], "C:/A/B/C"), Some(WorkspaceId(1)));
}

#[test]
fn component_boundary_is_respected() {
    assert_eq!(matched(&["C:/A/B"], "C:/A/BC"), None);
}

/// Backslash separators and case folding are equivalences only the Windows
/// path rules grant; on a case-sensitive filesystem these name different
/// directories.
#[cfg(windows)]
#[test]
fn exact_match_reuses_a_windows_spelling_of_the_same_path() {
    assert_eq!(
        exactly_matched(&["C:/A", "c:\\work\\project\\"], "C:/WORK/PROJECT"),
        Some(WorkspaceId(2))
    );
}

/// The ordered directories of `roots`, primary first.
fn ordered(roots: &WorkspaceRoots) -> Vec<&str> {
    roots.ordered().collect()
}

#[test]
fn an_equivalent_path_spelling_is_rejected_as_a_duplicate() {
    let mut roots = WorkspaceRoots::single("C:/Work/Project".into());

    // A `.` component and a trailing separator drop out on any platform.
    assert_eq!(roots.add("C:/Work/Project/.".into()), RootChange::Duplicate);
    assert_eq!(ordered(&roots), ["C:/Work/Project"]);
    assert_eq!(roots.add("C:/Work/Project/".into()), RootChange::Duplicate);
    assert_eq!(ordered(&roots), ["C:/Work/Project"]);

    #[cfg(windows)]
    {
        assert_eq!(
            roots.add(r"c:\work\project\\".into()),
            RootChange::Duplicate
        );
        assert_eq!(ordered(&roots), ["C:/Work/Project"]);
    }
}

#[test]
fn a_repeated_entry_in_a_saved_list_is_dropped_once() {
    let roots = WorkspaceRoots::new(
        "C:/A".into(),
        vec!["C:/B".into(), "C:/A/".into(), "C:/B/".into()],
    );

    assert_eq!(ordered(&roots), ["C:/A", "C:/B"]);

    #[cfg(windows)]
    {
        let roots = WorkspaceRoots::new(
            "C:/A".into(),
            vec!["C:/B".into(), r"c:\a".into(), "C:/B/".into()],
        );

        assert_eq!(ordered(&roots), ["C:/A", "C:/B"]);
    }
}

#[test]
fn making_a_directory_primary_preserves_every_other_position() {
    let mut roots = WorkspaceRoots::new(
        "C:/A".into(),
        vec!["C:/B".into(), "C:/C".into(), "C:/D".into()],
    );

    assert_eq!(roots.make_primary("C:/C"), RootChange::Applied);
    assert_eq!(ordered(&roots), ["C:/C", "C:/A", "C:/B", "C:/D"]);
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

    let id = manager.list().active_id();

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

#[test]
fn temporary_ids_include_pinned_normal_workspaces_but_not_settings() {
    let mut manager = manager(3, true);

    let second = WorkspaceId(2);
    let third = WorkspaceId(3);
    let settings = WorkspaceId(100);

    manager.set_temporary(second, true);

    manager.set_temporary(third, true);

    manager.set_temporary(settings, true);

    manager.set_pinned(second, true);

    assert_eq!(manager.temporary_ids().collect::<Vec<_>>(), [second, third]);
}

#[test]
fn a_workspace_row_toggles_between_all_tabs_and_none() {
    let mut manager = manager(2, false);

    let fold = |manager: &WorkspaceManager| {
        manager
            .summaries()
            .iter()
            .find(|summary| summary.id == WorkspaceId(2))
            .map(|summary| summary.tab_fold)
    };

    assert_eq!(fold(&manager), Some(TabFold::All));

    manager.cycle_tab_fold(WorkspaceId(2));

    assert_eq!(fold(&manager), Some(TabFold::Collapsed));

    manager.cycle_tab_fold(WorkspaceId(2));

    assert_eq!(fold(&manager), Some(TabFold::All));

    manager.cycle_tab_fold(WorkspaceId(2));

    assert_eq!(fold(&manager), Some(TabFold::Collapsed));

    // Each workspace folds on its own.
    assert_eq!(
        manager.summaries().first().map(|summary| summary.tab_fold),
        Some(TabFold::All)
    );
}
