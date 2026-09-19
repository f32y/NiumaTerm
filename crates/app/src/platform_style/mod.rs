//! Platform conventions shared by application chrome and conversation surfaces.
//!
//! Views are built once and shared by every platform. The measurements and
//! the few per-component adjustments that differ between hosts come from
//! [`PlatformStyle`], which `MacOs` and `Windows` implement. Both implementations
//! compile on every host, so editing one platform's appearance is type-checked
//! from the other; [`Host`] names the implementation this build targets and is
//! what views read.
//!
//! Add a hook here when a view needs a platform-specific measurement or
//! builder step, then answer it in both implementations. Views compose a
//! builder hook with `map`, for example `.map(Host::title_bar)`. Styling that
//! holds everywhere stays in the view, so a hook carries only what actually
//! differs between platforms.
//!
//! The `app` library owns this module so its conversation views and the
//! executable's window chrome use the same platform measurements and styling.

// Both implementations compile on every host, so a change to one platform's
// appearance is type-checked while the other one is built. That leaves the
// implementation this build does not target unreferenced.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod macos;
#[cfg_attr(target_os = "macos", allow(dead_code))]
mod windows;

use gpui::{AnyElement, Div, Stateful};
use gpui_component::TitleBar;
use gpui_component::button::Button;
use gpui_component::input::Input;
use gpui_component::tab::Tab;

#[cfg(target_os = "macos")]
use crate::platform_style::macos::MacOs;
#[cfg(not(target_os = "macos"))]
use crate::platform_style::windows::Windows;

/// What a tab still has room to draw. The close control outranks the tab
/// icon, which outranks the title: a tab nobody can close is worse than a tab
/// nobody can identify at a glance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabDensity {
    /// Icon, title, and the close control on hover.
    Full,
    /// Icon and the close control on hover; the title is dropped.
    Compact,
    /// A single glyph slot, shared by the icon and the close control.
    IconOnly,
}

pub trait PlatformStyle {
    /// Inset of the native window controls from the window's top-left corner
    /// when the host draws them over the leading edge of the title bar.
    /// Window creation anchors the controls there, and the sidebar starts its
    /// rows under the close button. `None` when the host draws its controls
    /// at the trailing edge as part of the bar.
    const WINDOW_CONTROLS_INSET: Option<f32>;

    /// Room at the leading edge of the title bar that the chrome's own
    /// leading region starts after. The region spans from there to the
    /// sidebar's edge, and the sidebar never shrinks past it.
    const TITLE_BAR_LEADING_INSET: f32;

    /// Margin after the title bar's trailing control group.
    const TITLE_BAR_TRAILING_INSET: f32;

    /// Below this width a tab collapses to the single glyph slot, because the
    /// leading icon, the close control and the padding around them no longer
    /// fit side by side.
    const COMPACT_TAB_WIDTH: f32;

    /// Below this width the title has under four characters of room left over
    /// from the icon, the padding and the close control, which renders as an
    /// ellipsis and little else, so the tab spends the width on the two
    /// controls instead.
    const FULL_TAB_WIDTH: f32;

    /// The title bar itself.
    fn title_bar(bar: TitleBar) -> TitleBar;

    /// One tab of the strip, at the density its width allows.
    fn tab(tab: Tab, density: TabDensity) -> Tab;

    /// The group holding a tab's leading icon.
    fn tab_prefix(prefix: Div) -> Div;

    /// The group holding a tab's trailing close control and progress bar.
    fn tab_suffix(suffix: Div, density: TabDensity) -> Div;

    /// The area holding a tab's title, which also carries its context menu.
    fn tab_title(title: Stateful<Div>, density: TabDensity) -> Stateful<Div>;

    /// The input that stands in for a tab's title while the tab is renamed.
    fn tab_rename_input(input: Input) -> Input;

    /// The title bar slot that names the session while tabs sit in the
    /// sidebar. It starts bottom-aligned, the edge the tab strip occupies.
    fn session_heading_slot(slot: Div) -> Div;

    /// The agent quota row as the sidebar's status area places it.
    fn sidebar_agent_usage(usage: AnyElement) -> AnyElement;

    /// The button that forms the agent quota row.
    fn agent_usage_row(row: Button) -> Button;
}

/// The conventions this build targets. Linux draws its window controls inside
/// the bar like Windows does and shares that implementation.
#[cfg(target_os = "macos")]
pub type Host = MacOs;

#[cfg(not(target_os = "macos"))]
pub type Host = Windows;
