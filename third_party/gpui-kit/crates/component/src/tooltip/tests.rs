use crate::tooltip::*;
use gpui::{point, size};

struct TooltipHost;

impl Render for TooltipHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().children((0..2usize).map(|row| {
            div()
                .id(("directory", row))
                .absolute()
                .left(px(20.))
                .top(px(100. + row as f32 * 44.))
                .w(px(220.))
                .h(px(32.))
                .child("Directory")
                .managed_tooltip_right(if row == 0 {
                    "Primary directory: /Users/test/work".to_string()
                } else {
                    format!(
                        r"Primary directory: C:\Users\test\{}",
                        r"long-directory\".repeat(8)
                    )
                })
        }))
    }
}

#[gpui::test]
fn side_tooltips_keep_hover_timing_and_clear_rows(cx: &mut gpui::TestAppContext) {
    cx.update(crate::init);
    let (root, cx) = cx.add_window_view(|window, cx| {
        let host = cx.new(|_| TooltipHost);
        Root::new(host, window, cx)
    });
    cx.simulate_resize(size(px(1200.), px(700.)));
    cx.run_until_parked();
    cx.refresh().unwrap();
    let overlay = root.read_with(cx, |root, _| root.tooltip_overlay.clone());
    cx.simulate_mouse_move(point(px(60.), px(116.)), None, gpui::Modifiers::default());
    cx.executor().advance_clock(Duration::from_millis(499));
    cx.run_until_parked();
    assert!(
        !overlay.read_with(cx, |overlay, _| overlay.has_content()),
        "first hover must retain its delay"
    );
    cx.executor().advance_clock(Duration::from_millis(1));
    cx.run_until_parked();
    assert!(overlay.read_with(cx, |overlay, _| overlay.has_content()));
    cx.executor().advance_clock(Duration::from_millis(160));
    cx.run_until_parked();
    cx.refresh().unwrap();
    let short = cx
        .debug_bounds("tooltip-view")
        .expect("tooltip was not painted");
    assert_eq!(short.left(), px(240.));
    assert!(short.size.width < px(720.));

    cx.simulate_mouse_move(point(px(60.), px(160.)), None, gpui::Modifiers::default());
    cx.run_until_parked();
    cx.refresh().unwrap();
    let wide = cx.debug_bounds("tooltip-view").unwrap();
    assert_eq!(wide.left(), px(240.));
    assert!(wide.size.width > px(420.));
    assert!(wide.size.width <= px(728.));

    cx.simulate_resize(size(px(640.), px(700.)));
    cx.run_until_parked();
    cx.refresh().unwrap();
    let narrow = cx.debug_bounds("tooltip-view").unwrap();
    assert_eq!(narrow.left(), px(240.));
    assert!(
        narrow.right() <= px(636.),
        "tooltip must fit without shifting across its row: {narrow:?}"
    );
    assert!(
        narrow.size.height > wide.size.height,
        "long paths wrap when side space is reduced"
    );
}
