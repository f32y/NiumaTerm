use std::collections::BTreeSet;

use crate::{EN, ZH_CN, i18n, init, parse_catalog, set_language};

#[test]
fn catalogs_have_identical_key_sets() {
    let en: BTreeSet<String> = parse_catalog(EN).into_keys().collect();
    let zh: BTreeSet<String> = parse_catalog(ZH_CN).into_keys().collect();
    let only_en: Vec<&String> = en.difference(&zh).collect();
    let only_zh: Vec<&String> = zh.difference(&en).collect();
    assert!(
        only_en.is_empty() && only_zh.is_empty(),
        "locale key sets differ; only in en: {only_en:?}, only in zh-CN: {only_zh:?}"
    );
}

// A single sequential test covers lookup and switching because the active
// language is process-global state; separate parallel tests would race on it.
#[test]
fn lookup_follows_active_language_and_falls_back_to_key() {
    init("en");
    assert_eq!(i18n("settings-appearance-language"), "Language");
    assert_eq!(i18n("no-such-key"), "no-such-key");

    set_language("zh-CN");
    assert_eq!(i18n("settings-appearance-language"), "语言");

    set_language("klingon");
    assert_eq!(i18n("settings-appearance-language"), "Language");
}
