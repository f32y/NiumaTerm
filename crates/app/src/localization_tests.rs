use std::collections::{BTreeMap, BTreeSet};

use rust_i18n::t;

const EN: &str = include_str!("../locales/en.toml");
const ZH_CN: &str = include_str!("../locales/zh-CN.toml");

fn catalog(source: &str) -> BTreeMap<String, String> {
    toml::from_str(source).expect("locale catalog must contain string values")
}

#[test]
fn catalogs_have_identical_keys_and_interpolation_names() {
    let en = catalog(EN);
    let zh = catalog(ZH_CN);

    assert_eq!(en.keys().collect::<Vec<_>>(), zh.keys().collect::<Vec<_>>());

    for (key, english) in &en {
        let placeholders = |text: &str| -> BTreeSet<String> {
            text.split("%{")
                .skip(1)
                .map(|part| {
                    part.split_once('}')
                        .expect("closed placeholder")
                        .0
                        .to_owned()
                })
                .collect()
        };

        assert_eq!(placeholders(english), placeholders(&zh[key]), "{key}");
    }
}

#[test]
fn generated_catalogs_preserve_every_translation() {
    for (locale, source) in [("en", EN), ("zh-CN", ZH_CN)] {
        for (key, expected) in catalog(source) {
            assert_eq!(
                t!(key.as_str(), locale = locale),
                expected,
                "{locale}: {key}"
            );
        }
    }
}

#[test]
fn unsupported_locales_fall_back_to_english_and_missing_keys_stay_visible() {
    assert_eq!(
        t!("settings-appearance-language", locale = "unknown"),
        "Language"
    );
    assert_eq!(t!("settings-appearance-language", locale = "zh-CN"), "语言");
    assert_eq!(t!("no-such-key", locale = "zh-CN"), "no-such-key");
}

#[test]
fn interpolation_handles_numbers_and_does_not_reinterpret_values() {
    assert_eq!(
        t!(
            "settings-agent-updates-title-numbered",
            locale = "en",
            provider = "Codex",
            ordinal = 2
        ),
        "Codex Updates 2"
    );
    assert_eq!(
        t!(
            "settings-agent-updates-title-numbered",
            locale = "zh-CN",
            provider = "Codex",
            ordinal = 2
        ),
        "Codex 更新 2"
    );
    assert_eq!(
        t!(
            "agent-context-used-left",
            locale = "en",
            tokens = "%{percent}",
            percent = 42
        ),
        "%{percent} used · 42% left"
    );
}
