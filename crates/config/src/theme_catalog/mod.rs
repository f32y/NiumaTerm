//! Theme families preserve the existing file identifiers used by saved settings.

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use crate::theme::{AppearanceTheme, Theme};

pub struct ThemeChoice {
    pub id: String,
    pub theme: Theme,
}

pub struct ThemeFamily {
    pub name: String,
    pub variants: Vec<ThemeChoice>,
}

impl ThemeFamily {
    pub fn variant(&self, mode: AppearanceTheme) -> &ThemeChoice {
        self.variants
            .iter()
            .find(|choice| choice.theme.mode == mode)
            .unwrap_or(&self.variants[0])
    }

    pub fn contains(&self, id: &str) -> bool {
        self.variants.iter().any(|choice| choice.id == id)
    }

    pub fn supports_both_modes(&self) -> bool {
        self.variants.len() == 2
    }
}

/// Only explicit family metadata joins files. Ambiguous duplicate modes stay
/// individually selectable, as do older custom themes without family metadata.
pub fn theme_families(themes: Vec<(String, Theme)>) -> Vec<ThemeFamily> {
    let mut groups: BTreeMap<String, Vec<ThemeChoice>> = BTreeMap::new();
    let mut families = Vec::new();

    for (id, theme) in themes {
        let family = theme.family.trim();

        if family.is_empty() {
            families.push(ThemeFamily {
                name: theme.name.clone(),
                variants: vec![ThemeChoice { id, theme }],
            });
        } else {
            groups
                .entry(family.to_owned())
                .or_default()
                .push(ThemeChoice { id, theme });
        }
    }

    for (name, variants) in groups {
        if variants.len() == 2 && variants[0].theme.mode != variants[1].theme.mode {
            families.push(ThemeFamily { name, variants });
        } else {
            families.extend(variants.into_iter().map(|choice| ThemeFamily {
                name: choice.theme.name.clone(),
                variants: vec![choice],
            }));
        }
    }

    families.sort_by_cached_key(|family| family.name.to_lowercase());

    families
}
