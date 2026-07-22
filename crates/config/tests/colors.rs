use nmt_config::colors::*;
use nmt_config::render_types::Color;

#[test]
fn test_conversion_from_hex_invalid_character() {
    let invalid_character_color =
        match ColorBuilder::from_hex(String::from("#invalid-color"), Format::SRGB0_255) {
            Ok(d) => d.to_string(),
            Err(e) => e,
        };

    assert_eq!(invalid_character_color, "Error: Character is not valid");
}

#[test]
fn test_conversion_from_hex_invalid_size() {
    let invalid_invalid_size = match ColorBuilder::from_hex(String::from("abc"), Format::SRGB0_255)
    {
        Ok(d) => d.to_string(),
        Err(e) => e,
    };

    assert_eq!(invalid_invalid_size, "Error: Hex String size is not valid");
}

#[test]
fn test_conversion_from_hex_sgb_255() {
    let color: Color = ColorBuilder::from_hex(String::from("#151515"), Format::SRGB0_1)
        .unwrap()
        .to_wgpu();
    assert_eq!(
        color,
        ColorWGPU {
            r: 0.08235294117647059,
            g: 0.08235294117647059,
            b: 0.08235294117647059,
            a: 1.0
        }
    );

    let color = ColorBuilder::from_hex(String::from("#FFFFFF"), Format::SRGB0_1).unwrap();
    assert_eq!(
        color,
        ColorBuilder {
            red: 1.0,
            green: 1.0,
            blue: 1.0,
            alpha: 1.0
        }
    );
}

#[test]
fn test_conversion_from_hex_sgb_1() {
    let color: Color = ColorBuilder::from_hex(String::from("#151515"), Format::SRGB0_255)
        .unwrap()
        .to_wgpu();
    assert_eq!(
        color,
        ColorWGPU {
            r: 21.0,
            g: 21.0,
            b: 21.0,
            a: 1.0
        }
    );

    let color = ColorBuilder::from_hex(String::from("#FFFFFF"), Format::SRGB0_255).unwrap();
    assert_eq!(
        color,
        ColorBuilder {
            red: 255.0,
            green: 255.0,
            blue: 255.0,
            alpha: 1.0
        }
    );
}

#[test]
fn test_conversion_from_gray_hex_with_alpha() {
    let color_with_alpha =
        ColorBuilder::from_hex(String::from("#15151580"), Format::SRGB0_255).unwrap();
    assert_eq!(
        color_with_alpha,
        ColorBuilder {
            red: 21.0,
            green: 21.0,
            blue: 21.0,
            alpha: 128.0 / 255.0
        }
    );

    let color_with_alpha_srgb0_1 =
        ColorBuilder::from_hex(String::from("#15151580"), Format::SRGB0_1).unwrap();
    assert_eq!(
        color_with_alpha_srgb0_1,
        ColorBuilder {
            red: 21.0 / 255.0,
            green: 21.0 / 255.0,
            blue: 21.0 / 255.0,
            alpha: 128.0 / 255.0
        }
    );
}

#[test]
fn test_conversion_from_teal_hex_with_alpha() {
    let color_with_alpha =
        ColorBuilder::from_hex(String::from("#06a49b99"), Format::SRGB0_255).unwrap();
    assert_eq!(
        color_with_alpha,
        ColorBuilder {
            red: 6.0,
            green: 164.0,
            blue: 155.0,
            alpha: 153.0 / 255.0
        }
    );

    let color_with_alpha_srgb0_1 =
        ColorBuilder::from_hex(String::from("#06a49b99"), Format::SRGB0_1).unwrap();
    assert_eq!(
        color_with_alpha_srgb0_1,
        ColorBuilder {
            red: 6.0 / 255.0,
            green: 164.0 / 255.0,
            blue: 155.0 / 255.0,
            alpha: 153.0 / 255.0
        }
    );
}
