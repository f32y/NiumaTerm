use nmt_config::Config;
use nmt_config::appearance::{AppearanceConfig, InputStyle};
use toml::from_str;

#[test]
fn appearance_section_defaults_when_absent() {
    let config: Config = from_str("").unwrap();
    assert_eq!(config.appearance, AppearanceConfig::default());
    assert_eq!(config.appearance.input_style, InputStyle::Waterfall);
    assert!(config.profiles.list.is_empty());
}
