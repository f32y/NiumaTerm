use nmt_config::layout::*;
use serde::Deserialize;
use toml::from_str;

#[test]
fn test_margin_from_css_single_value() {
    let margin = Margin::from_css_values(&[10.0]).unwrap();
    assert_eq!(margin.top, 10.0);
    assert_eq!(margin.right, 10.0);
    assert_eq!(margin.bottom, 10.0);
    assert_eq!(margin.left, 10.0);
}

#[test]
fn test_margin_from_css_two_values() {
    let margin = Margin::from_css_values(&[10.0, 5.0]).unwrap();
    assert_eq!(margin.top, 10.0);
    assert_eq!(margin.right, 5.0);
    assert_eq!(margin.bottom, 10.0);
    assert_eq!(margin.left, 5.0);
}

#[test]
fn test_margin_from_css_four_values() {
    let margin = Margin::from_css_values(&[10.0, 5.0, 15.0, 20.0]).unwrap();
    assert_eq!(margin.top, 10.0);
    assert_eq!(margin.right, 5.0);
    assert_eq!(margin.bottom, 15.0);
    assert_eq!(margin.left, 20.0);
}

#[test]
fn test_margin_from_css_invalid_count() {
    let result = Margin::from_css_values(&[10.0, 5.0, 15.0]);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err(),
        "Invalid margin format: expected 1, 2, or 4 values, got 3"
    );
}

#[test]
fn test_margin_deserialize_single() {
    let toml_str = r#"margin = [10]"#;
    #[derive(Deserialize)]
    struct Config {
        margin: Margin,
    }
    let config: Config = from_str(toml_str).unwrap();
    assert_eq!(config.margin.top, 10.0);
    assert_eq!(config.margin.right, 10.0);
    assert_eq!(config.margin.bottom, 10.0);
    assert_eq!(config.margin.left, 10.0);
}

#[test]
fn test_margin_deserialize_invalid() {
    let toml_str = r#"margin = [10, 5, 15]"#;
    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct Config {
        margin: Margin,
    }
    let result: Result<Config, _> = from_str(toml_str);
    assert!(result.is_err());
}

#[test]
fn test_panel_deserialize_full() {
    let toml_str = r#"
        [panel]
        margin = [8]
        row-gap = 2
        column-gap = 3
    "#;

    #[derive(Deserialize)]
    struct Config {
        panel: Panel,
    }

    let config: Config = from_str(toml_str).unwrap();
    assert_eq!(config.panel.margin, Margin::all(8.0));
    assert_eq!(config.panel.row_gap, 2.0);
    assert_eq!(config.panel.column_gap, 3.0);
}
