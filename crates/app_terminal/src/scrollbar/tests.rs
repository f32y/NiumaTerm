use crate::scrollbar::*;

#[test]
fn scrollbar_opacity_fades_after_linger() {
    assert_eq!(SCROLLBAR_AUTO_HIDE_DELAY, time::Duration::from_millis(500));
    assert_eq!(
        SCROLLBAR_FADE_OUT_DURATION,
        time::Duration::from_millis(200)
    );
    assert_eq!(scrollbar_opacity(true, None), Some(1.0));
    assert_eq!(
        scrollbar_opacity(false, Some(SCROLLBAR_AUTO_HIDE_DELAY / 2)),
        Some(1.0)
    );

    let fading = scrollbar_opacity(
        false,
        Some(SCROLLBAR_AUTO_HIDE_DELAY + SCROLLBAR_FADE_OUT_DURATION / 2),
    )
    .unwrap();
    assert!(fading > 0.0 && fading < 1.0);

    assert_eq!(
        scrollbar_opacity(
            false,
            Some(SCROLLBAR_AUTO_HIDE_DELAY + SCROLLBAR_FADE_OUT_DURATION),
        ),
        None
    );
}

#[test]
fn scrollbar_thumb_stays_inside_track_with_long_history() {
    let (top, height) = scrollbar_thumb_geometry(10_000.0, 9_975.0, 25.0).unwrap();

    assert!(top + height <= 1.0, "thumb bottom was {}", top + height);
    assert_eq!(
        scrollbar_offset_for_thumb(10_000.0, 25.0, top),
        Some(9_975.0)
    );
}
