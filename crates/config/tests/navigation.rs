use nmt_config::colors::hex_to_color_arr;
use nmt_config::navigation::{Navigation, NavigationMode};
use serde::Deserialize;
use toml::from_str;

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct Root {
    #[serde(default = "Navigation::default")]
    navigation: Navigation,
}

#[test]
fn test_plain() {
    let content = r#"
        [navigation]
        mode = 'Plain'
    "#;

    let decoded = from_str::<Root>(content).unwrap();
    assert_eq!(decoded.navigation.mode, NavigationMode::Plain);
    assert!(!decoded.navigation.clickable);
    assert!(decoded.navigation.color_automation.is_empty());
}

#[test]
fn test_color_automation() {
    let content = r#"
        [navigation]
        mode = 'Tab'
        color-automation = [
            { program = 'vim', color = '#333333' }
        ]
    "#;

    let decoded = from_str::<Root>(content).unwrap();
    assert_eq!(decoded.navigation.mode, NavigationMode::Tab);
    assert!(!decoded.navigation.clickable);
    assert!(!decoded.navigation.color_automation.is_empty());
    assert_eq!(
        decoded.navigation.color_automation[0].program,
        "vim".to_string()
    );
    assert_eq!(decoded.navigation.color_automation[0].path, String::new());
    assert_eq!(
        decoded.navigation.color_automation[0].color,
        hex_to_color_arr("#333333")
    );
}
