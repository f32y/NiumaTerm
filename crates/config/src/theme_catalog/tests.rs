use crate::builtin_themes::THEMES;
use crate::theme::{AppearanceTheme, Theme};
use crate::theme_catalog::theme_families;

#[test]
fn paired_builtins_keep_legacy_ids_when_switching_modes() {
    let themes = THEMES
        .iter()
        .map(|builtin| {
            (
                builtin.name.to_owned(),
                toml::from_str(builtin.source).unwrap(),
            )
        })
        .collect();

    let families = theme_families(themes);

    for prefix in ["slate", "fluent", "modern", "claude"] {
        let light = format!("{prefix}_light");
        let dark = format!("{prefix}_dark");

        let family = families
            .iter()
            .find(|family| family.contains(&light))
            .unwrap();

        assert!(family.contains(&dark));
        assert!(family.supports_both_modes());
        assert_eq!(family.variant(AppearanceTheme::Light).id, light);
        assert_eq!(family.variant(AppearanceTheme::Dark).id, dark);
    }

    assert_eq!(
        families
            .iter()
            .map(|family| family.variants.len())
            .sum::<usize>(),
        THEMES.len()
    );
}

#[test]
fn custom_themes_are_not_paired_by_filename() {
    let themes = ["custom_light", "custom_dark"].map(|id| {
        (
            id.to_owned(),
            Theme {
                name: id.to_owned(),
                ..Theme::default()
            },
        )
    });

    let families = theme_families(themes.into());

    assert_eq!(families.len(), 2);
    assert!(families.iter().all(|family| !family.supports_both_modes()));
}

#[test]
fn incomplete_and_ambiguous_families_keep_every_file_selectable() {
    let themes = [
        ("one", AppearanceTheme::Light),
        ("two", AppearanceTheme::Light),
        ("three", AppearanceTheme::Dark),
    ]
    .map(|(id, mode)| {
        (
            id.to_owned(),
            Theme {
                name: id.to_owned(),
                family: "Custom".into(),
                mode,
                ..Theme::default()
            },
        )
    });

    let families = theme_families(themes.into());

    assert_eq!(families.len(), 3);
    assert!(families.iter().all(|family| !family.supports_both_modes()));

    let single = &families[0];

    assert_eq!(
        single.variant(AppearanceTheme::Dark).id,
        single.variants[0].id
    );
}
