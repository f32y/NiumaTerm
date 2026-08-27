use crate::tabs::*;

/// Build a manager of fake surfaces (u32) with sequential ids 1..=n, ids
/// equal to the surface value for easy assertions.
fn manager(n: u32) -> TabManager<u32> {
    let mut mgr = TabManager::new(1, TabId(1), "PowerShell".into());
    for i in 2..=n {
        mgr.new_tab(i, TabId(i as u64), "PowerShell".into());
    }
    mgr
}

#[test]
fn new_tab_becomes_active() {
    let mut mgr = manager(1);
    assert_eq!(mgr.active_id(), TabId(1));
    mgr.new_tab(2, TabId(2), "PowerShell".into());
    assert_eq!(mgr.active_index(), 1);
    assert_eq!(mgr.active_id(), TabId(2));
    assert_eq!(*mgr.active(), 2);
}

#[test]
fn new_tabs_use_their_profile_names() {
    let mut mgr = manager(1);
    assert_eq!(mgr.tabs()[0].title(), "PowerShell");
    mgr.new_tab(2, TabId(2), "Command Prompt".into());
    mgr.new_tab(3, TabId(3), "Developer PowerShell".into());
    assert_eq!(mgr.tabs()[1].title(), "Command Prompt");
    assert_eq!(mgr.tabs()[2].title(), "Developer PowerShell");
}

#[test]
fn close_is_refused_for_single_tab() {
    let mut mgr = manager(1);
    assert!(mgr.close(TabId(1)).is_none());
    assert_eq!(mgr.len(), 1);
}

#[test]
fn close_active_falls_to_right_neighbor() {
    let mut mgr = manager(3); // active = tab3 (index 2)
    mgr.activate(1); // active = tab2 (index 1)
    let removed = mgr.close(TabId(2));
    assert_eq!(removed, Some(2));
    // tab3 was to the right; it is now active at index 1.
    assert_eq!(mgr.active_id(), TabId(3));
    assert_eq!(mgr.active_index(), 1);
}

#[test]
fn close_active_with_no_right_neighbor_falls_left() {
    let mut mgr = manager(3); // active = tab3 (index 2, rightmost)
    let removed = mgr.close(TabId(3));
    assert_eq!(removed, Some(3));
    assert_eq!(mgr.active_id(), TabId(2));
    assert_eq!(mgr.active_index(), 1);
}

#[test]
fn closing_left_of_active_keeps_active_tab() {
    let mut mgr = manager(3);
    mgr.activate(2); // active = tab3
    mgr.close(TabId(1)); // closes a tab left of active
    assert_eq!(mgr.active_id(), TabId(3));
    assert_eq!(mgr.active_index(), 1);
}

#[test]
fn focus_next_and_prev_wrap_around() {
    let mut mgr = manager(3);
    mgr.activate(2);
    mgr.focus_next();
    assert_eq!(mgr.active_index(), 0); // wrapped to first
    mgr.focus_prev();
    assert_eq!(mgr.active_index(), 2); // wrapped to last
}

#[test]
fn reorder_moves_tab_and_active_follows() {
    let mut mgr = manager(3); // [t1, t2, t3], active t3
    mgr.activate(0); // active = t1
    mgr.reorder(0, 2); // move t1 to the end -> [t2, t3, t1]
    assert_eq!(mgr.tabs()[0].id(), TabId(2));
    assert_eq!(mgr.tabs()[2].id(), TabId(1));
    // active still t1, now at index 2.
    assert_eq!(mgr.active_id(), TabId(1));
    assert_eq!(mgr.active_index(), 2);
}

#[test]
fn terminal_title_replaces_default_and_empty_restores_it() {
    let mut mgr = manager(2);
    assert!(mgr.set_title(TabId(1), "vim".into()));
    assert_eq!(mgr.tabs()[0].title(), "vim");
    assert_eq!(mgr.tabs()[1].title(), "PowerShell");
    assert!(mgr.set_title(TabId(1), String::new()));
    assert_eq!(mgr.tabs()[0].title(), "PowerShell");
}

#[test]
fn user_title_takes_precedence_over_terminal_title() {
    let mut mgr = manager(1);
    mgr.set_title(TabId(1), "vim".into());
    mgr.rename(TabId(1), "editor".into());

    assert_eq!(mgr.tabs()[0].title(), "editor");
    assert_eq!(mgr.tabs()[0].user_title(), Some("editor"));
    assert!(!mgr.set_title(TabId(1), "shell".into()));
    assert_eq!(mgr.tabs()[0].title(), "editor");
}

#[test]
fn mark_exited_keeps_tab_and_flags_it() {
    let mut mgr = manager(2);
    mgr.mark_exited(TabId(1));
    assert!(mgr.tabs()[0].exited());
    assert_eq!(mgr.len(), 2);
}

#[test]
fn bell_flags_a_tab_until_it_is_activated() {
    let mut mgr = manager(2); // tab 2 is active
    mgr.ring_bell(TabId(1));

    assert!(mgr.tabs()[0].bell());
    // Clearing acts on the active tab, so the ringing one keeps its flag.
    assert!(!mgr.clear_active_bell());
    assert!(mgr.tabs()[0].bell());

    mgr.activate(0);

    assert!(mgr.clear_active_bell());
    assert!(!mgr.tabs()[0].bell());
    assert!(!mgr.clear_active_bell());
}

#[test]
fn a_failure_survives_the_successes_that_follow_it() {
    let mut mgr = manager(2); // tab 2 is active

    mgr.record_outcome(TabId(1), CommandOutcome::from_exit_code(Some(1)));
    mgr.record_outcome(TabId(1), CommandOutcome::from_exit_code(Some(0)));

    assert_eq!(mgr.tabs()[0].last_outcome(), Some(CommandOutcome::Failed));

    // Clearing acts on the active tab, so the flagged one keeps its result
    // until the user goes there.
    assert!(!mgr.clear_active_outcome());

    mgr.activate(0);

    assert!(mgr.clear_active_outcome());
    assert_eq!(mgr.tabs()[0].last_outcome(), None);
    assert!(!mgr.clear_active_outcome());
}

#[test]
fn an_unreported_exit_code_is_not_a_failure() {
    assert_eq!(
        CommandOutcome::from_exit_code(None),
        CommandOutcome::Succeeded
    );
}

#[test]
fn progress_state_zero_clears_the_bar() {
    let mut mgr = manager(1);
    let set = ProgressReport {
        state: ProgressState::Set,
        progress: Some(42),
    };

    mgr.set_progress(TabId(1), set);

    assert_eq!(mgr.tabs()[0].progress(), Some(set));

    mgr.set_progress(
        TabId(1),
        ProgressReport {
            state: ProgressState::Remove,
            progress: None,
        },
    );

    assert_eq!(mgr.tabs()[0].progress(), None);

    mgr.set_progress(TabId(1), set);
    mgr.clear_progress(TabId(1));

    assert_eq!(mgr.tabs()[0].progress(), None);
}

#[test]
fn id_is_stable_across_close_and_reorder() {
    let mut mgr = manager(3);
    mgr.close(TabId(1)); // indices shift, ids do not
    assert_eq!(mgr.tabs()[0].id(), TabId(2));
    mgr.reorder(0, 1);
    assert_eq!(mgr.tabs()[1].id(), TabId(2));
    // tab3 keeps its id throughout.
    assert!(mgr.tabs().iter().any(|t| t.id() == TabId(3)));
}
