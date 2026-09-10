use crate::images::{MAX_IMAGE_EDGE, scaled_dimensions};
#[test]
fn an_oversized_image_is_shrunk_onto_the_cap_keeping_its_shape() {
    assert_eq!(scaled_dimensions(3840, 2160), Some((MAX_IMAGE_EDGE, 1152)));
    assert_eq!(scaled_dimensions(1000, 4000), Some((512, MAX_IMAGE_EDGE)));

    // Already within the cap on both edges.
    assert_eq!(scaled_dimensions(800, 600), None);
    assert_eq!(scaled_dimensions(MAX_IMAGE_EDGE, 10), None);
}

#[test]
fn encoded_storage_moves_into_preview_and_survives_reordering_without_copying() {
    use std::io::Cursor;
    use std::sync::Arc;

    use image_rs::{DynamicImage, ImageFormat, RgbaImage};

    use crate::images::PendingAttachments;

    let mut source = Vec::new();
    DynamicImage::ImageRgba8(RgbaImage::new(4, 4))
        .write_to(&mut Cursor::new(&mut source), ImageFormat::Png)
        .unwrap();
    let mut images = PendingAttachments::default();
    let mut pointer = 0;
    images
        .attach(&source, |bytes| {
            pointer = bytes.as_ptr() as usize;
            Arc::new(bytes)
        })
        .ok()
        .unwrap();
    images.attach(&source, Arc::new).ok().unwrap();
    let retained = images.iter().next().unwrap().image.clone();
    assert_eq!(retained.as_ptr() as usize, pointer);
    assert_eq!(
        images.reconcile("[Image #2] then [Image #1]").as_deref(),
        Some("[Image #1] then [Image #2]")
    );
    assert!(Arc::ptr_eq(&images.iter().nth(1).unwrap().image, &retained));
    images.reconcile("[Image #2]");
    assert!(Arc::ptr_eq(&images.iter().next().unwrap().image, &retained));
    assert_eq!(
        images.iter().next().unwrap().image.as_ptr() as usize,
        pointer
    );
}
