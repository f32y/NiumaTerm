pub struct BuiltinTheme {
    pub name: &'static str,
    pub source: &'static str,
}

pub const THEMES: &[BuiltinTheme] = &[
    BuiltinTheme {
        name: "default_dark",
        source: include_str!("default_dark.toml"),
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
        assert_eq!(THEMES.len(), 2);
        assert_ne!(THEMES[0].name, THEMES[1].name);
        assert!(THEMES.iter().all(|theme| !theme.source.is_empty()));
    }
}
