use gpui::{Bounds, point, px, size};

use crate::agent_tab::transcript::render::image_preview::{preview_bounds, preview_size};

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

#[gpui::test]
fn closing_retains_the_image_and_reset_discards_the_preview(cx: &mut gpui::TestAppContext) {
    use std::io::Cursor;
    use std::sync::Arc;

    use gpui::{AppContext as _, Image, ImageFormat};

    use crate::agent_tab::AgentKind;
    use crate::agent_tab::settings::AgentSettings;
    use crate::agent_tab::transcript::TranscriptView;
    use crate::agent_tab::transcript::render::image_preview::ImagePreview;

    let mut bytes = Cursor::new(Vec::new());

    image_rs::DynamicImage::new_rgba8(1, 1)
        .write_to(&mut bytes, image_rs::ImageFormat::Png)
        .unwrap();

    let image = Arc::new(Image::from_bytes(ImageFormat::Png, bytes.into_inner()));

    cx.set_global(AgentSettings::default());

    let cx = cx.add_empty_window();
    let view = cx.update(|_, cx| cx.new(|_| TranscriptView::new(AgentKind::Codex, None)));

    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.zoom_image(image.clone(), None, cx);

            view.preview.close_zoomed_image(cx);

            assert!(
                matches!(&view.preview.image_preview, ImagePreview::Closing(preview)
            if Arc::ptr_eq(&preview.image, &image))
            );

            view.zoom_image(image.clone(), None, cx);

            view.reset_presentation();

            assert!(matches!(view.preview.image_preview, ImagePreview::Closed));
        })
    });
}
