use crate::builtin_themes::*;

#[test]
fn builtins_have_unique_names_and_sources() {
    assert!(!THEMES.is_empty());
    let mut names: Vec<_> = THEMES.iter().map(|theme| theme.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), THEMES.len());
    assert!(THEMES.iter().all(|theme| !theme.source.is_empty()));
}
