use crate::tab::TabBar;
use crate::tab::tab::*;
use crate::theme::Theme;
use gpui::{Context, Render, TestAppContext, VisualTestContext};

const VARIANTS: [TabVariant; 5] = [
    TabVariant::Tab,
    TabVariant::Outline,
    TabVariant::Pill,
    TabVariant::Segmented,
    TabVariant::Underline,
];

const LONG_LABEL: &str = "Account Settings & Preferences";

/// One [`TabBar`], optionally capped, holding the tab `build` returns.
struct TabBarTest {
    variant: TabVariant,
    max_width: Option<Pixels>,
    build: fn(Tab) -> Tab,
}

impl Render for TabBarTest {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        TabBar::new("tabs")
            .with_variant(self.variant)
            .selected_index(0)
            .when_some(self.max_width, |this, width| this.max_width(width))
            .child((self.build)(
                Tab::new().debug_selector(|| "tab".to_string()),
            ))
    }
}

fn show(
    cx: &mut TestAppContext,
    variant: TabVariant,
    max_width: Option<Pixels>,
    build: fn(Tab) -> Tab,
) -> &mut VisualTestContext {
    cx.update(crate::init);
    let (_, cx) = cx.add_window_view(|_, _| TabBarTest {
        variant,
        max_width,
        build,
    });
    cx.run_until_parked();
    cx
}

/// Outer width of a labelled tab in every variant, capped or not.
fn variant_widths(
    cx: &mut TestAppContext,
    build: fn(Tab) -> Tab,
    max_width: Option<Pixels>,
) -> Vec<Pixels> {
    VARIANTS
        .into_iter()
        .map(|variant| {
            show(cx, variant, max_width, build)
                .debug_bounds("tab")
                .expect("tab not rendered")
                .size
                .width
        })
        .collect()
}

#[gpui::test]
fn a11y_label_defaults_to_visible_label(_cx: &mut TestAppContext) {
    let tab = Tab::new().label("Account");

    assert_eq!(tab.a11y_label(), Some("Account".into()));
}

#[gpui::test]
fn explicit_a11y_label_overrides_visible_label(_cx: &mut TestAppContext) {
    let tab = Tab::new().label("Acct").aria_label("Account settings");

    assert_eq!(tab.a11y_label(), Some("Account settings".into()));
}

#[gpui::test]
fn max_width_leaves_short_tabs_untouched(cx: &mut TestAppContext) {
    // The box the cap wraps the label in must not report a different
    // intrinsic width than the bare label it replaces.
    let build: fn(Tab) -> Tab = |tab| tab.label("Go");

    let uncapped = variant_widths(cx, build, None);
    let capped = variant_widths(cx, build, Some(px(200.)));

    assert_eq!(uncapped, capped, "a tab under the cap must not be resized");
}

#[gpui::test]
fn max_width_caps_long_tabs(cx: &mut TestAppContext) {
    let build: fn(Tab) -> Tab = |tab| tab.label(LONG_LABEL);

    let uncapped = variant_widths(cx, build, None);
    let capped = variant_widths(cx, build, Some(px(120.)));

    for (variant, (uncapped, capped)) in VARIANTS.into_iter().zip(uncapped.into_iter().zip(capped))
    {
        assert!(
            uncapped > px(120.),
            "{variant:?} is not long enough to exercise the cap ({uncapped:?})"
        );
        assert!(
            capped <= px(120.),
            "{variant:?} width {capped:?} exceeds max_width"
        );
    }
}

#[gpui::test]
fn max_width_keeps_prefix_and_suffix_intact(cx: &mut TestAppContext) {
    let cx = show(cx, TabVariant::Segmented, Some(px(140.)), |tab| {
        tab.prefix(Icon::new(IconName::BookOpen))
            .label(LONG_LABEL)
            .suffix(div().size(px(16.)).debug_selector(|| "suffix".to_string()))
    });

    let tab = cx.debug_bounds("tab").expect("tab not rendered");
    let suffix = cx.debug_bounds("suffix").expect("suffix not rendered");

    assert!(tab.size.width <= px(140.));
    assert_eq!(
        suffix.size.width,
        px(16.),
        "the label should absorb the truncation, not the suffix"
    );
    assert!(
        suffix.right() <= tab.right(),
        "suffix must stay within the tab"
    );
    // A wrapping tab pushes the suffix onto a second line, which the fixed
    // tab height then clips: it keeps its size and stays inside the tab,
    // but lands back at the left edge instead of after the label.
    assert!(
        suffix.left() > tab.center().x,
        "suffix must follow the label, not wrap below it"
    );
}

/// Icon-only tabs are sized to a square by construction, so the cap has to
/// leave them alone however narrow it is.
#[gpui::test]
fn max_width_exempts_icon_only_tabs(cx: &mut TestAppContext) {
    let build: fn(Tab) -> Tab = |tab| tab.icon(Icon::new(IconName::BookOpen));
    let width = |cx: &mut TestAppContext, max_width| {
        show(cx, TabVariant::Tab, max_width, build)
            .debug_bounds("tab")
            .expect("tab not rendered")
            .size
            .width
    };

    let uncapped = width(cx, None);
    assert!(
        uncapped > px(16.),
        "the cap has to be narrower than the icon tab to be meaningful"
    );
    assert_eq!(
        width(cx, Some(px(16.))),
        uncapped,
        "an icon-only tab must ignore max_width"
    );
}

#[gpui::test]
fn modern_variant_reserves_border_space_in_every_state(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        cx.set_global(Theme::default());
        let variant = TabVariant::Modern;

        // Selected draws a 1px outline; the other states must reserve the
        // same border so activating a tab does not shift its content.
        let selected = variant.selected(cx).borders;
        assert_eq!(variant.normal(cx).borders, selected);
        assert_eq!(variant.hovered(false, cx).borders, selected);
        assert_eq!(variant.disabled(true, cx).borders, selected);
        assert_eq!(variant.disabled(false, cx).borders, selected);
    });
}
