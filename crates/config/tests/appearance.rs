use nmt_config::Config;
use nmt_config::appearance::{AppearanceConfig, InputStyle};

#[test]
fn appearance_section_defaults_when_absent() {
    let config: Config = toml::from_str("").unwrap();
    assert_eq!(config.appearance, AppearanceConfig::default());
    assert_eq!(config.appearance.input_style, InputStyle::Waterfall);
    assert!(config.profiles.list.is_empty());
}
