pub struct BuiltinTheme {
    pub name: &'static str,
    pub source: &'static str,
}

pub const THEMES: &[BuiltinTheme] = &[
    BuiltinTheme {
        name: "modern_dark",
        source: include_str!("modern_dark.toml"),
    },
    BuiltinTheme {
        name: "modern_light",
        source: include_str!("modern_light.toml"),
    },
    BuiltinTheme {
        name: "ubuntu",
        source: include_str!("ubuntu.toml"),
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
        assert_eq!(THEMES.len(), 3);
        let mut names: Vec<_> = THEMES.iter().map(|theme| theme.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), THEMES.len());
        assert!(THEMES.iter().all(|theme| !theme.source.is_empty()));
    }
}
