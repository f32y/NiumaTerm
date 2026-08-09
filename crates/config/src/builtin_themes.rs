//! Built-in theme sources compiled into the binary so a fresh install renders
//! with a full palette before any user theme file exists.

pub struct BuiltinTheme {
    pub name: &'static str,
    pub source: &'static str,
}

pub const THEMES: &[BuiltinTheme] = &[
    BuiltinTheme {
        name: "modern_dark",
        source: include_str!("builtin_themes/modern_dark.toml"),
    },
    BuiltinTheme {
        name: "modern_light",
        source: include_str!("builtin_themes/modern_light.toml"),
    },
    BuiltinTheme {
        name: "modern_gray",
        source: include_str!("builtin_themes/modern_gray.toml"),
    },
    BuiltinTheme {
        name: "ubuntu",
        source: include_str!("builtin_themes/ubuntu.toml"),
    },
];

pub fn get(name: &str) -> Option<&'static str> {
    THEMES
        .iter()
        .find(|theme| theme.name == name)
        .map(|theme| theme.source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_have_unique_names_and_sources() {
        assert!(!THEMES.is_empty());
        let mut names: Vec<_> = THEMES.iter().map(|theme| theme.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), THEMES.len());
        assert!(THEMES.iter().all(|theme| !theme.source.is_empty()));
    }
}
