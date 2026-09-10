use crate::workspace::AgentWorkspace;

#[test]
fn stored_history_signatures_keep_root_spelling_case_and_order() {
    #[cfg(windows)]
    let (paths, expected) = (
        vec![" C:/ÄBC/ ".into(), "./Relative/./".into(), "C:/".into()],
        "c:/\\/Äbc\nrelative\nc:/\\",
    );

    #[cfg(unix)]
    let (paths, expected) = (
        vec![" /ÄBC/ ".into(), "./Relative/./".into(), "/".into()],
        "//ÄBC\nRelative\n/",
    );

    let workspace = AgentWorkspace::new(Some("primary".into()), paths);

    assert_eq!(workspace.history_signature(), expected);
}

#[test]
fn ordered_directories_start_with_the_primary_one() {
    let workspace = AgentWorkspace::new(Some("C:/A".into()), vec!["C:/B".into(), "C:/C".into()]);

    assert_eq!(workspace.primary(), Some("C:/A"));
    assert_eq!(workspace.additional(), ["C:/B", "C:/C"]);
    assert_eq!(
        workspace.ordered().collect::<Vec<_>>(),
        ["C:/A", "C:/B", "C:/C"]
    );
    assert!(workspace.is_multi_root());
}

#[test]
fn a_single_directory_conversation_reports_no_additional_access() {
    let workspace = AgentWorkspace::single(Some("C:/A".into()));

    assert_eq!(workspace.ordered().collect::<Vec<_>>(), ["C:/A"]);
    assert!(!workspace.is_multi_root());

    // An unchanged signature is what keeps every existing single-directory
    // input history reachable.
    assert!(workspace.history_signature().is_empty());
}

#[test]
fn additional_directories_without_a_primary_one_are_dropped() {
    let workspace = AgentWorkspace::new(None, vec!["C:/B".into()]);

    assert_eq!(workspace.primary(), None);
    assert!(workspace.additional().is_empty());
    assert_eq!(workspace.ordered().count(), 0);
}

#[test]
fn redundant_segments_share_one_input_history_signature() {
    let a = AgentWorkspace::new(Some("C:/A".into()), vec!["C:/B".into(), "C:/C/".into()]);
    let b = AgentWorkspace::new(Some("C:/A".into()), vec!["C:/B/.".into(), "C:/C".into()]);

    assert_eq!(a.history_signature(), b.history_signature());
}

/// Backslash separators and case folding are equivalences only the Windows
/// path rules grant; on a case-sensitive filesystem these are distinct
/// directories and must keep distinct histories.
#[cfg(windows)]
#[test]
fn windows_spellings_share_one_input_history_signature() {
    let a = AgentWorkspace::new(Some("C:/A".into()), vec!["C:/B".into(), "C:/C/".into()]);
    let b = AgentWorkspace::new(Some("C:/A".into()), vec![r"c:\B\.".into(), r"C:\c".into()]);

    assert_eq!(a.history_signature(), b.history_signature());
}

#[test]
fn a_different_root_set_has_a_different_signature() {
    let a = AgentWorkspace::new(Some("C:/A".into()), vec!["C:/B".into()]);
    let b = AgentWorkspace::new(Some("C:/A".into()), vec!["C:/D".into()]);

    // Order is part of the identity: the same directories in another order are
    // another workspace configuration.
    let c = AgentWorkspace::new(Some("C:/A".into()), vec!["C:/D".into(), "C:/B".into()]);

    assert_ne!(a.history_signature(), b.history_signature());
    assert_ne!(b.history_signature(), c.history_signature());
}
