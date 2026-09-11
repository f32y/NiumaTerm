use crate::ui::{DEFAULT_CJK_FONT_FAMILY, font_with_default_fallback};

#[test]
fn default_font_prefers_one_chinese_face_for_missing_glyphs() {
    let font = font_with_default_fallback("Menlo");

    let fallbacks = font
        .fallbacks
        .expect("default fallback should be configured");

    assert_eq!(font.family, "Menlo");
    assert_eq!(fallbacks.fallback_list(), [DEFAULT_CJK_FONT_FAMILY]);
}

/// A face this platform does not have does not fail to load — it silently
/// resolves to something else, which is how a terminal ends up drawing its
/// grid in Helvetica. Every default named as a family therefore has to be one
/// the host installs.
///
/// The UI face is exempt: `.SystemUIFont` is a token GPUI maps to the
/// backend's own system face, and macOS deliberately keeps that face out of
/// the family list.
#[test]
fn every_default_font_family_is_installed() {
    use font_kit::source::SystemSource;

    use crate::ui::settings::{DEFAULT_FONT_FAMILY, DEFAULT_UI_FONT};

    let source = SystemSource::new();

    for family in [DEFAULT_FONT_FAMILY, DEFAULT_CJK_FONT_FAMILY] {
        assert!(
            source.select_family_by_name(family).is_ok(),
            "{family} is a default but is not installed on this platform"
        );
    }

    assert!(
        DEFAULT_UI_FONT.starts_with('.') || source.select_family_by_name(DEFAULT_UI_FONT).is_ok(),
        "{DEFAULT_UI_FONT} is neither a GPUI font token nor an installed family"
    );
}
