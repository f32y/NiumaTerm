use gpui::{
    App, Bounds, Pixels, Rgba, TextAlign, TextRun, Window, fill, point, px, rgb, rgba, size,
};

use crate::block_list::FrozenItemChrome;
use crate::metrics;
use crate::theme::{BLOCK_GUTTER_GAP, BLOCK_GUTTER_WIDTH, BLOCK_SELECTED_TINT, SEPARATOR_COLOR};

pub(crate) fn paint_frozen_separators(
    bounds: Bounds<Pixels>,
    separators: &[f32],
    window: &mut Window,
) {
    let left = bounds.left() - px(metrics::PADDING_PX);
    let right = bounds.right() + px(metrics::PADDING_PX);
    for y in separators {
        window.paint_quad(fill(
            Bounds::new(
                point(left, bounds.top() + px(*y)),
                size(right - left, px(1.0)),
            ),
            Rgba {
                r: ((SEPARATOR_COLOR >> 16) & 0xff) as f32 / 255.0,
                g: ((SEPARATOR_COLOR >> 8) & 0xff) as f32 / 255.0,
                b: (SEPARATOR_COLOR & 0xff) as f32 / 255.0,
                a: 0.67,
            },
        ));
    }
}

pub(crate) fn paint_frozen_chrome(
    bounds: Bounds<Pixels>,
    items_chrome: &[FrozenItemChrome],
    window: &mut Window,
    cx: &mut App,
) {
    for chrome in items_chrome {
        let top = bounds.top() + px(chrome.top);
        let height = px(chrome.bottom - chrome.top);
        let gutter_alpha = if chrome.selected { 0xe6 } else { 0x59 };

        window.paint_quad(fill(
            Bounds::new(
                point(
                    bounds.left() - px(BLOCK_GUTTER_GAP + BLOCK_GUTTER_WIDTH),
                    top,
                ),
                size(px(BLOCK_GUTTER_WIDTH), height),
            ),
            rgba((chrome.accent << 8) | gutter_alpha),
        ));

        if chrome.selected {
            window.paint_quad(fill(
                Bounds::new(point(bounds.left(), top), size(bounds.size.width, height)),
                rgba(BLOCK_SELECTED_TINT),
            ));
        }
    }

    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());

    for chrome in items_chrome {
        let Some(header) = chrome.header.as_deref() else {
            continue;
        };

        let runs = [TextRun {
            len: header.len(),
            font: style.font(),
            color: rgb(0x7f8c98).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }];

        let shaped = window.text_system().shape_line(
            header.to_string().into(),
            font_size,
            &runs,
            Some(bounds.size.width),
        );

        let _ = shaped.paint(
            point(bounds.left(), bounds.top() + px(chrome.header_y)),
            px(0.0),
            TextAlign::Right,
            Some(bounds.size.width),
            window,
            cx,
        );
    }
}
