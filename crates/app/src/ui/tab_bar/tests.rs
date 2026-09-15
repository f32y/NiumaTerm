use nmt_config::profile::Profile;

use crate::ui::tab_bar::menu::profile_root_choices;

/// A terminal Profile named `name` running `shell`.
fn profile(name: &str, shell: &str) -> Profile {
    Profile {
        name: name.to_string(),
        shell: shell.to_string(),
        args: String::new(),
    }
}

/// Reachable workspace directories in the given order.
fn roots(paths: &[&str]) -> Vec<(String, bool)> {
    paths.iter().map(|path| (path.to_string(), true)).collect()
}

#[test]
fn combinations_are_profile_major_and_cover_every_directory() {
    let profiles = [profile("P1", "pwsh.exe"), profile("P2", "cmd.exe")];
    let choices = profile_root_choices(&profiles, &roots(&["C:/A", "C:/B", "C:/C"]));

    assert_eq!(choices.len(), 6);

    let pairs: Vec<_> = choices
        .iter()
        .map(|choice| (choice.launch.0.as_deref().unwrap(), choice.cwd.as_str()))
        .collect();

    assert_eq!(
        pairs,
        [
            ("pwsh.exe", "C:/A"),
            ("pwsh.exe", "C:/B"),
            ("pwsh.exe", "C:/C"),
            ("cmd.exe", "C:/A"),
            ("cmd.exe", "C:/B"),
            ("cmd.exe", "C:/C"),
        ]
    );
}

#[test]
fn a_profile_without_a_command_contributes_no_combination() {
    let profiles = [
        profile("P1", "pwsh.exe"),
        profile("Empty", "   "),
        profile("P2", "cmd.exe"),
    ];

    let choices = profile_root_choices(&profiles, &roots(&["C:/A", "C:/B"]));

    assert_eq!(choices.len(), 4);
    assert!(choices.iter().all(|choice| choice.label.contains('P')));
}

#[test]
fn each_combination_launches_in_exactly_the_selected_directory() {
    let profiles = [profile("P1", "pwsh.exe")];
    let choices = profile_root_choices(&profiles, &roots(&[r"C:\Work\api", r"D:\Docs\api"]));

    // Two directories sharing a final component stay distinguishable because
    // every label carries the full path.
    assert_eq!(choices[0].cwd, r"C:\Work\api");
    assert_eq!(choices[1].cwd, r"D:\Docs\api");
    assert!(choices[0].label.contains(r"C:\Work\api"));
    assert!(choices[1].label.contains(r"D:\Docs\api"));
    assert_ne!(choices[0].label, choices[1].label);
    assert!(choices.iter().all(|choice| choice.label.contains("P1")));
}

#[test]
fn combinations_of_an_unavailable_directory_stay_listed_and_disabled() {
    let profiles = [profile("P1", "pwsh.exe"), profile("P2", "cmd.exe")];

    let roots = vec![
        ("C:/A".to_string(), true),
        ("Z:/detached".to_string(), false),
    ];

    let choices = profile_root_choices(&profiles, &roots);

    assert_eq!(choices.len(), 4);

    let enabled: Vec<_> = choices.iter().map(|choice| choice.enabled).collect();

    assert_eq!(enabled, [true, false, true, false]);
}
