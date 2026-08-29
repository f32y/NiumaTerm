//! Built-in theme sources compiled into the binary so a fresh install renders
//! with a full palette before any user theme file exists.

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
        name: "modern_gray",
        source: include_str!("modern_gray.toml"),
    },
    BuiltinTheme {
        name: "ubuntu",
        source: include_str!("ubuntu.toml"),
    },
    BuiltinTheme {
        name: "fluent_dark",
        source: include_str!("fluent_dark.toml"),
    },
    BuiltinTheme {
        name: "fluent_light",
        source: include_str!("fluent_light.toml"),
    },
    BuiltinTheme {
        name: "warm_light",
        source: include_str!("warm_light.toml"),
    },
];

pub fn get(name: &str) -> Option<&'static str> {
    THEMES
        .iter()
        .find(|theme| theme.name == name)
        .map(|theme| theme.source)
}

#[cfg(test)]
mod tests;
