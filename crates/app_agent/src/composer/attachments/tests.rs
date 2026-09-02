use std::io::Cursor;

use gpui::{Image, ImageFormat};

use crate::composer::attachments::{
    AttachError, MAX_ATTACHMENTS, MAX_IMAGE_EDGE, PendingAttachments, placeholder_text,
    scaled_dimensions,
};

/// A real encoded PNG, because attaching decodes what it is given.
fn png(width: u32, height: u32) -> Image {
    let buffer = image_rs::RgbaImage::from_pixel(width, height, image_rs::Rgba([9, 9, 9, 255]));
    let mut bytes = Vec::new();

    image_rs::DynamicImage::ImageRgba8(buffer)
        .write_to(&mut Cursor::new(&mut bytes), image_rs::ImageFormat::Png)
        .expect("encode test png");

    Image::from_bytes(ImageFormat::Png, bytes)
}

fn attach_three() -> (PendingAttachments, String) {
    let mut pending = PendingAttachments::default();
    let mut text = String::new();

    for _ in 0..3 {
        let placeholder = pending.attach(&png(4, 4)).ok().expect("attach");
        text.push_str(&placeholder);
    }

    (pending, text)
}

#[test]
fn attaching_numbers_placeholders_in_order() {
    let (pending, text) = attach_three();

    assert_eq!(text, "[Image #1][Image #2][Image #3]");
    assert_eq!(pending.iter().count(), 3);
}

#[test]
fn removing_the_first_attachment_renumbers_the_rest() {
    let (mut pending, text) = attach_three();
    let removed = pending.placeholder_at(0).expect("placeholder").to_string();
    let edited = text.replace(&removed, "");

    let rewritten = pending.reconcile(&edited).expect("renumbered");

    assert_eq!(rewritten, "[Image #1][Image #2]");
    assert_eq!(pending.iter().count(), 2);
}

#[test]
fn removing_a_middle_attachment_keeps_the_text_around_it() {
    let mut pending = PendingAttachments::default();
    let mut text = String::new();
    for (index, word) in ["one ", "two ", "three "].iter().enumerate() {
        let placeholder = pending.attach(&png(4, 4)).ok().expect("attach");
        text.push_str(word);
        text.push_str(&placeholder);
        assert_eq!(placeholder, placeholder_text(index + 1));
    }

    let removed = pending.placeholder_at(1).expect("placeholder").to_string();
    let edited = text.replace(&removed, "");
    let rewritten = pending.reconcile(&edited).expect("renumbered");

    assert_eq!(rewritten, "one [Image #1]two three [Image #2]");
    assert_eq!(pending.iter().count(), 2);
}

#[test]
fn deleting_every_placeholder_drops_every_attachment() {
    let (mut pending, _) = attach_three();

    assert_eq!(pending.reconcile(""), None);
    assert!(pending.is_empty());
}

#[test]
fn a_placeholder_naming_no_attachment_is_left_as_text() {
    let mut pending = PendingAttachments::default();
    let placeholder = pending.attach(&png(4, 4)).ok().expect("attach");
    let text = format!("{placeholder} and a typed [Image #7]");

    // Nothing to renumber, and the typed one is the user's text to send.
    assert_eq!(pending.reconcile(&text), None);
    assert_eq!(pending.iter().count(), 1);
}

#[test]
fn moving_a_placeholder_reorders_the_attachments() {
    let (mut pending, _) = attach_three();
    let first = pending.iter().next().expect("first").bytes().to_vec();

    // The first image now reads last, so it is numbered last.
    let rewritten = pending
        .reconcile("[Image #2][Image #3][Image #1]")
        .expect("renumbered");

    assert_eq!(rewritten, "[Image #1][Image #2][Image #3]");
    assert_eq!(
        pending.iter().last().expect("last").bytes(),
        first.as_slice()
    );
}

#[test]
fn only_placeholders_naming_an_attachment_are_links() {
    let mut pending = PendingAttachments::default();
    let placeholder = pending.attach(&png(4, 4)).ok().expect("attach");
    let text = format!("look at {placeholder} and a typed [Image #7]");

    assert_eq!(pending.placeholder_links(&text), vec![8..18]);
    assert_eq!(&text[8..18], placeholder);
}

#[test]
fn a_link_resolves_to_the_image_its_placeholder_names() {
    let (pending, text) = attach_three();
    let second = pending.iter().nth(1).expect("second").bytes().to_vec();

    let links = pending.placeholder_links(&text);
    let image = pending
        .linked_image(&text, links[1].clone())
        .expect("linked image");

    assert_eq!(image.bytes(), second.as_slice());
}

#[test]
fn a_range_naming_no_placeholder_resolves_to_nothing() {
    let (pending, text) = attach_three();

    // What a range from an earlier reading of the text looks like once an
    // edit has moved the placeholders out from under it.
    assert!(pending.linked_image(&text, 3..13).is_none());
}

#[test]
fn a_message_carries_no_more_than_the_cap() {
    let mut pending = PendingAttachments::default();
    for _ in 0..MAX_ATTACHMENTS {
        assert!(pending.attach(&png(4, 4)).is_ok());
    }

    assert!(matches!(pending.attach(&png(4, 4)), Err(AttachError::Full)));
    assert_eq!(pending.iter().count(), MAX_ATTACHMENTS);
}

#[test]
fn an_oversized_image_is_shrunk_onto_the_cap_keeping_its_shape() {
    assert_eq!(scaled_dimensions(3840, 2160), Some((MAX_IMAGE_EDGE, 1152)));
    assert_eq!(scaled_dimensions(1000, 4000), Some((512, MAX_IMAGE_EDGE)));

    // Already within the cap on both edges.
    assert_eq!(scaled_dimensions(800, 600), None);
    assert_eq!(scaled_dimensions(MAX_IMAGE_EDGE, 10), None);
}

#[test]
fn attaching_shrinks_an_oversized_image() {
    let mut pending = PendingAttachments::default();
    let (from_width, from_height) = (MAX_IMAGE_EDGE + 400, 512);

    pending
        .attach(&png(from_width, from_height))
        .ok()
        .expect("attach");

    let attached = image_rs::load_from_memory(pending.iter().next().expect("attached").bytes())
        .expect("decode attached");
    let (width, height) = (attached.width(), attached.height());

    // Onto the cap, not past it, with the shape kept. The resize fits the
    // image inside the target box rather than stretching to it, so the long
    // edge can land a pixel or two short of the cap.
    assert!(width.max(height) <= MAX_IMAGE_EDGE);
    assert!(width.max(height) > MAX_IMAGE_EDGE - 4);

    let from_ratio = f64::from(from_width) / f64::from(from_height);
    let to_ratio = f64::from(width) / f64::from(height);
    assert!((from_ratio - to_ratio).abs() < 0.01);
}

#[test]
fn bytes_that_are_not_an_image_do_not_attach() {
    let mut pending = PendingAttachments::default();

    assert!(matches!(
        pending.attach(&Image::from_bytes(ImageFormat::Png, b"not a png".to_vec())),
        Err(AttachError::Undecodable)
    ));
    assert!(pending.is_empty());
}
