mod scrollable;

/// Time a scrolling-mode scrollbar stays opaque after activity.
pub const SCROLLBAR_AUTO_HIDE_DELAY: std::time::Duration = std::time::Duration::from_millis(500);
/// Time a scrolling-mode scrollbar takes to fade away.
pub const SCROLLBAR_FADE_OUT_DURATION: std::time::Duration = std::time::Duration::from_millis(200);

pub use gpui_base::AutoScroll;
pub use gpui_base::ScrollableMask;
pub use gpui_base::{
    Scrollbar, ScrollbarAxis, ScrollbarEntrance, ScrollbarHandle, ScrollbarMode, ScrollbarMotion,
    ScrollbarStyles, ScrollbarThumbStyle, ScrollbarTrackStyle,
};
pub use scrollable::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_scrollbar_is_the_base_type() {
        fn accepts_base(_: gpui_base::Scrollbar) {}
        fn accepts_handle(_: impl gpui_base::ScrollbarHandle) {}

        let handle = gpui::ScrollHandle::default();
        let scrollbar: crate::scroll::Scrollbar = Scrollbar::vertical(&handle).styles(|styles| {
            styles
                .track(|style| style.bg(gpui::transparent_black()))
                .thumb(|style| style.bg(gpui::transparent_black()))
                .thumb_hover(|style| style.bg(gpui::transparent_black()))
                .thumb_active(|style| style.bg(gpui::transparent_black()))
        });
        accepts_base(scrollbar);
        accepts_handle(handle);
    }
}
