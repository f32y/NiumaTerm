use gpui::px;

use crate::ui::right_panel::PanelSizing;

#[test]
fn default_auxiliary_width_balances_wide_content_and_preserves_narrow_reading_space() {
    let mut sizing = PanelSizing {
        available: px(1000.0),
        preferred: None,
    };

    assert!((sizing.width().as_f32() - 381.966).abs() < 0.01);

    sizing.available = px(2000.0);

    assert_eq!(sizing.width(), px(480.0));

    sizing.available = px(500.0);

    assert_eq!(sizing.width(), px(180.0));
    assert_eq!(sizing.available - sizing.width(), px(320.0));

    sizing.available = px(280.0);

    assert_eq!(sizing.width(), px(0.0));
}

#[test]
fn manual_panel_width_survives_window_shrink_and_expansion() {
    let mut sizing = PanelSizing {
        available: px(1200.0),
        preferred: None,
    };

    sizing.resize(px(420.0));

    assert_eq!(sizing.width(), px(420.0));

    sizing.available = px(600.0);

    assert_eq!(sizing.width(), px(280.0));

    sizing.available = px(1200.0);

    assert_eq!(sizing.width(), px(420.0));

    sizing.resize(px(2000.0));

    assert_eq!(sizing.width(), px(480.0));
}
