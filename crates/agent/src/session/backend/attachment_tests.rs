use std::{fs, iter};

use crate::session::ImageAttachment;
use crate::session::backend::{inline_images, write_attachments};
use crate::session::tests::Scratch;

#[test]
fn inline_images_keep_byte_order_and_media_types() {
    let images = inline_images(
        [
            ImageAttachment {
                bytes: &[1, 2, 3],
                media_type: "image/png",
            },
            ImageAttachment {
                bytes: &[9, 8],
                media_type: "image/jpeg",
            },
        ]
        .into_iter(),
    );

    assert_eq!(images.len(), 2);
    assert_eq!(images[0].bytes, [1, 2, 3]);
    assert_eq!(images[0].media_type, "image/png");
    assert_eq!(images[1].bytes, [9, 8]);
    assert_eq!(images[1].media_type, "image/jpeg");
}

#[test]
fn disk_images_preserve_positions_skip_failed_writes_and_reuse_paths() {
    let scratch = Scratch::new();
    fs::create_dir(scratch.0.join("image-2.png")).unwrap();
    let images = [
        ImageAttachment {
            bytes: &[1],
            media_type: "image/png",
        },
        ImageAttachment {
            bytes: &[2],
            media_type: "image/png",
        },
        ImageAttachment {
            bytes: &[3],
            media_type: "image/png",
        },
    ];

    let paths = write_attachments(images.into_iter(), &scratch.0);

    assert_eq!(
        paths,
        [scratch.0.join("image-1.png"), scratch.0.join("image-3.png")]
    );
    assert_eq!(fs::read(&paths[0]).unwrap(), [1]);
    assert_eq!(fs::read(&paths[1]).unwrap(), [3]);

    let replacement = write_attachments(
        iter::once(ImageAttachment {
            bytes: &[4, 5],
            media_type: "image/png",
        }),
        &scratch.0,
    );

    assert_eq!(replacement, [paths[0].clone()]);
    assert_eq!(fs::read(&replacement[0]).unwrap(), [4, 5]);
}

#[test]
fn empty_images_do_not_create_a_directory_and_unusable_scratch_returns_no_paths() {
    let scratch = Scratch::new();
    let unused = scratch.0.join("unused");

    assert!(write_attachments(iter::empty(), &unused).is_empty());
    assert!(!unused.exists());

    let blocked = scratch.0.join("file");
    fs::write(&blocked, b"occupied").unwrap();

    assert!(
        write_attachments(
            iter::once(ImageAttachment {
                bytes: b"image",
                media_type: "image/png",
            }),
            &blocked
        )
        .is_empty()
    );
    assert_eq!(fs::read(blocked).unwrap(), b"occupied");
}
