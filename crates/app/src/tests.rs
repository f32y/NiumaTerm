use crate::*;

#[test]
fn keeps_url_argument_for_normal_launch() {
    let StartupArgs { url, .. } =
        parse_startup_args_from(["NiumaTerm", "nmt://action/new_tab?path=C%3A%2FWorkspace"]);
    assert_eq!(
        url.as_deref(),
        Some("nmt://action/new_tab?path=C%3A%2FWorkspace")
    );
}

#[test]
fn parses_shell_extension_path_flags() {
    let StartupArgs { url, .. } = parse_startup_args_from(["NiumaTerm", "--new-tab", r"C:\A Dir"]);
    assert_eq!(
        url.as_deref(),
        Some("nmt://action/new_tab?path=C%3A%5CA%20Dir")
    );

    let StartupArgs { url, .. } = parse_startup_args_from(["NiumaTerm", "--new-window", r"C:\A&B"]);
    assert_eq!(
        url.as_deref(),
        Some("nmt://action/new_window?path=C%3A%5CA%26B")
    );
}

#[test]
fn parses_testing_mode() {
    let StartupArgs { url, testing, .. } = parse_startup_args_from(["NiumaTerm", "--testing"]);
    assert!(testing);
    assert!(url.is_none());

    let StartupArgs { testing, .. } = parse_startup_args_from(["NiumaTerm"]);
    assert!(!testing);
}

#[test]
fn parses_profiling_flag() {
    let StartupArgs { profiling, .. } =
        parse_startup_args_from(["NiumaTerm", "--enable-profiling"]);
    assert!(profiling);

    let StartupArgs { profiling, .. } = parse_startup_args_from(["NiumaTerm"]);
    assert!(!profiling);
}
