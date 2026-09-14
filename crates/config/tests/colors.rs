use nmt_config::colors::*;

#[test]
fn test_conversion_from_hex_invalid_character() {
    let invalid_character_color = match Rgba::from_hex("#invalid-color".into()) {
        Ok(d) => d.to_string(),
        Err(e) => e,
    };

    assert_eq!(invalid_character_color, "Error: Character is not valid");
}

#[test]
fn test_conversion_from_hex_invalid_size() {
    let invalid_invalid_size = match Rgba::from_hex("abc".into()) {
        Ok(d) => d.to_string(),
        Err(e) => e,
    };

    assert_eq!(invalid_invalid_size, "Error: Hex String size is not valid");
}

#[test]
fn test_conversion_from_hex_sgb_255() {
    let color = Rgba::from_hex("#151515".into()).unwrap();

    assert_eq!(
        color,
        Rgba {
            red: 0.08235294117647059,
            green: 0.08235294117647059,
            blue: 0.08235294117647059,
            alpha: 1.0
        }
    );

    let color = Rgba::from_hex("#FFFFFF".into()).unwrap();

    assert_eq!(
        color,
        Rgba {
            red: 1.0,
            green: 1.0,
            blue: 1.0,
            alpha: 1.0
        }
    );
}

#[test]
fn test_conversion_from_gray_hex_with_alpha() {
    let color_with_alpha = Rgba::from_hex("#15151580".into()).unwrap();

    assert_eq!(
        color_with_alpha,
        Rgba {
            red: 21.0 / 255.0,
            green: 21.0 / 255.0,
            blue: 21.0 / 255.0,
            alpha: 128.0 / 255.0
        }
    );
}

#[test]
fn test_conversion_from_teal_hex_with_alpha() {
    let color_with_alpha = Rgba::from_hex("#06a49b99".into()).unwrap();

    assert_eq!(
        color_with_alpha,
        Rgba {
            red: 6.0 / 255.0,
            green: 164.0 / 255.0,
            blue: 155.0 / 255.0,
            alpha: 153.0 / 255.0
        }
    );
}
