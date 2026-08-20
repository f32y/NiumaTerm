use crate::ui::font_with_default_fallback;

#[test]
fn default_font_prefers_microsoft_yahei_for_missing_glyphs() {
    let font = font_with_default_fallback("Consolas");
    let fallbacks = font
        .fallbacks
        .expect("default fallback should be configured");

    assert_eq!(font.family, "Consolas");
    assert_eq!(fallbacks.fallback_list(), ["Microsoft YaHei"]);
}
