use gpui::{Bounds, point, px, size};

use crate::transcript::render::image_preview::{preview_bounds, preview_size};

#[test]
fn an_image_the_stream_has_room_for_keeps_its_own_size() {
    let shown = preview_size(size(px(200.), px(100.)), size(px(1000.), px(800.)));
    assert_eq!(shown, size(px(200.), px(100.)));
}

#[test]
fn a_wide_image_fits_the_width_and_keeps_its_shape() {
    let shown = preview_size(size(px(2000.), px(500.)), size(px(1000.), px(800.)));
    assert_eq!(shown, size(px(800.), px(200.)));
}

#[test]
fn a_tall_image_fits_the_height_and_keeps_its_shape() {
    let shown = preview_size(size(px(500.), px(2000.)), size(px(1000.), px(800.)));
    assert_eq!(shown, size(px(160.), px(640.)));
}

#[test]
fn a_preview_leaves_its_thumbnail_and_arrives_at_its_full_size() {
    let thumbnail = Bounds::new(point(px(10.), px(20.)), size(px(56.), px(56.)));
    let full = Bounds::new(point(px(100.), px(200.)), size(px(400.), px(300.)));

    assert_eq!(preview_bounds(thumbnail, full, 0.0), thumbnail);
    assert_eq!(preview_bounds(thumbnail, full, 1.0), full);
    assert_eq!(
        preview_bounds(thumbnail, full, 0.5),
        Bounds::new(point(px(55.), px(110.)), size(px(228.), px(178.)))
    );
}
