use nmt_config::hints::*;
use onig::Regex as OnigRegex;
use toml::{from_str, to_string};

#[test]
fn test_hints_default() {
    let hints = Hints::default();
    assert_eq!(hints.alphabet, DEFAULT_HINTS_ALPHABET);
    assert_eq!(hints.rules.len(), 1);

    let default_hint = &hints.rules[0];
    assert!(default_hint.regex.is_some());
    assert!(default_hint.hyperlinks);
    assert!(default_hint.post_processing);
    assert!(!default_hint.persist);
}

#[test]
fn test_hint_serialization() {
    let hint = Hint {
        regex: Some("test.*pattern".to_string()),
        hyperlinks: false,
        post_processing: true,
        persist: false,
        action: HintAction::Action {
            action: HintInternalAction::Copy,
        },
        mouse: HintMouse::default(),
        binding: None,
    };

    let serialized = to_string(&hint).unwrap();
    let deserialized: Hint = from_str(&serialized).unwrap();
    assert_eq!(hint, deserialized);
}

/// Given input text, return every leftmost non-overlapping match produced
/// by `DEFAULT_URL_REGEX`. Used to verify the path branches.
fn find_all(input: &str) -> Vec<&str> {
    let re = OnigRegex::new(DEFAULT_URL_REGEX).unwrap();
    re.find_iter(input).map(|(s, e)| &input[s..e]).collect()
}

#[test]
fn test_default_regex_matches_schemed_urls() {
    assert_eq!(
        find_all("visit https://rioterm.com here"),
        vec!["https://rioterm.com"]
    );
    assert_eq!(find_all("file://foo"), vec!["file://foo"]);
}

#[test]
fn test_default_regex_matches_rooted_paths() {
    // Dotted paths (file-like): match stops at the next non-dotted token.
    assert_eq!(find_all("open ~/notes.md please"), vec!["~/notes.md"],);
    assert_eq!(find_all("see ./script.sh"), vec!["./script.sh"]);
    assert_eq!(
        find_all("check ../parent/file.txt"),
        vec!["../parent/file.txt"],
    );

    // Non-dotted (directory-like): absorbs trailing spaces+words because
    // the path could be a directory whose name contains spaces (e.g.
    // `~/Desktop please/...`). This matches ghostty's behavior.
    assert_eq!(find_all("open ~/Desktop please"), vec!["~/Desktop please"]);
    assert_eq!(find_all("cd /tmp/foo"), vec!["/tmp/foo"]);
    assert_eq!(find_all("logs at $HOME/logs"), vec!["$HOME/logs"]);
}

#[test]
fn test_default_regex_matches_bare_relative_paths_with_extension() {
    assert_eq!(find_all("edit src/main.rs now"), vec!["src/main.rs"]);
    assert_eq!(
        find_all("see app/rioterm/src/hints.rs"),
        vec!["app/rioterm/src/hints.rs"]
    );
}

#[test]
fn test_default_regex_rejects_midword_slash() {
    // Lookbehind `(?<![\w~/])/` keeps the `/` inside `foo/bar` from
    // anchoring the rooted-path branch. Branch 3 also fails (no dot).
    assert!(find_all("foo/bar").is_empty());
}

#[test]
fn test_default_regex_rejects_midword_tilde() {
    // Lookbehind `(?<!\w)~/` rejects the `~/bar` inside `foo~/bar`.
    assert!(find_all("foo~/bar").is_empty());
}

#[test]
fn test_default_regex_strips_trailing_punctuation_on_urls() {
    // `(?<![,.])` excludes the trailing period.
    assert_eq!(
        find_all("see https://example.com."),
        vec!["https://example.com"],
    );
}

#[test]
fn test_default_regex_matches_dot_prefixed_paths() {
    // `.config/foo.txt` matches the `.word/` branch (hidden dirs).
    assert_eq!(
        find_all(".config/rio/config.toml"),
        vec![".config/rio/config.toml"]
    );
}

#[test]
fn test_default_regex_prefers_bare_relative_over_embedded_slash() {
    // `Compiling src/config/url.zig` — the bare-relative branch anchors
    // at `src/...` and wins over the rooted `/config/url.zig` because
    // it starts earlier in the text.
    assert_eq!(
        find_all("Compiling src/config/url.zig"),
        vec!["src/config/url.zig"],
    );
}

#[test]
fn test_config_with_hints() {
    use nmt_config::Config;

    let config_toml = r#"
[hints]
alphabet = "abcdef"

[[hints.rules]]
regex = "test.*pattern"
hyperlinks = false
post-processing = true
persist = false

[hints.rules.action]
action = "Copy"

[hints.rules.binding]
key = "T"
mods = ["Control"]
"#;

    let config: Config = from_str(config_toml).unwrap();
    assert_eq!(config.hints.alphabet, "abcdef");
    assert_eq!(config.hints.rules.len(), 1);

    let hint = &config.hints.rules[0];
    assert_eq!(hint.regex, Some("test.*pattern".to_string()));
    assert!(!hint.hyperlinks);
    assert!(hint.post_processing);
    assert!(!hint.persist);
}
