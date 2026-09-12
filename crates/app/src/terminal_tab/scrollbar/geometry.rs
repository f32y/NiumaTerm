use std::time;

pub(crate) const SCROLLBAR_AUTO_HIDE_DELAY: time::Duration = time::Duration::from_millis(500);

pub(crate) const SCROLLBAR_FADE_OUT_DURATION: time::Duration = time::Duration::from_millis(200);

pub(crate) fn scrollbar_thumb_geometry(total: f64, offset: f64, len: f64) -> Option<(f32, f32)> {
    if total <= len {
        return None;
    }

    let thumb_height = (len / total).clamp(0.03, 1.0) as f32;
    let scrollable = total - len;
    let thumb_top = (offset.clamp(0.0, scrollable) / scrollable) as f32 * (1.0 - thumb_height);

    Some((thumb_top, thumb_height))
}

pub(crate) fn scrollbar_offset_for_thumb(total: f64, len: f64, thumb_top: f32) -> Option<f64> {
    let (_, thumb_height) = scrollbar_thumb_geometry(total, 0.0, len)?;
    let thumb_travel = 1.0 - thumb_height;

    Some((thumb_top / thumb_travel).clamp(0.0, 1.0) as f64 * (total - len))
}

pub(crate) fn scrollbar_opacity(
    dragging: bool,
    elapsed_since_scroll: Option<time::Duration>,
) -> Option<f32> {
    if dragging {
        return Some(1.0);
    }

    let elapsed = elapsed_since_scroll?;

    if elapsed < SCROLLBAR_AUTO_HIDE_DELAY {
        return Some(1.0);
    }

    if elapsed >= SCROLLBAR_AUTO_HIDE_DELAY + SCROLLBAR_FADE_OUT_DURATION {
        return None;
    }

    let fade_elapsed = elapsed - SCROLLBAR_AUTO_HIDE_DELAY;

    Some(1.0 - fade_elapsed.as_secs_f32() / SCROLLBAR_FADE_OUT_DURATION.as_secs_f32())
}
