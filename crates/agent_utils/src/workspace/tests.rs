use crate::workspace::AgentWorkspace;

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
fn equivalent_spellings_share_one_input_history_signature() {
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
