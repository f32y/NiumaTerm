use rust_i18n::{locale, set_locale};

// Locale changes are process-global, so this integration executable keeps
// switching separate from the application tests that render English labels.
#[test]
fn language_switches_update_the_app_and_component_catalogs_together() {
    set_locale("en");

    let english = app::_rust_i18n_try_translate(&locale(), "settings-appearance-language")
        .expect("English language label");

    assert_eq!(english, "Language");

    gpui_component::set_locale("zh-CN");

    assert_eq!(&*locale(), "zh-CN");
    assert_eq!(
        app::_rust_i18n_try_translate(&locale(), "settings-appearance-language")
            .expect("Chinese language label"),
        "语言"
    );
    assert_eq!(english, "Language");

    set_locale("en");

    assert_eq!(&*gpui_component::locale(), "en");
    assert_eq!(
        app::_rust_i18n_try_translate(&locale(), "settings-appearance-language")
            .expect("restored English language label"),
        "Language"
    );
}
