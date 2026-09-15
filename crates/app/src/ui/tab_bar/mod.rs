//! Tab bars in both layouts: the horizontal strip in the title bar and the
//! vertical rows listed under each workspace in the sidebar. The two layouts
//! share the new-tab menu, drag previews, and progress colors kept here.

pub(super) use crate::ui::tab_bar::horizontal::{TabDensity, TabStrip, tab_shape_preview};
pub(super) use crate::ui::tab_bar::menu::new_tab_menu;
pub(super) use crate::ui::tab_bar::vertical::{VerticalTabList, WorkspaceTabs, accept_row_drops};

pub(super) mod menu;

mod drag;
mod horizontal;
mod vertical;

#[cfg(test)]
mod tests;

use gpui::{App, Hsla};
use gpui_component::ActiveTheme as _;
use nmt_terminal::event::{ProgressReport, ProgressState};

/// Color and fill of an OSC 9;4 progress track. Shared by the title-bar strip
/// and the sidebar's tab rows so one report reads the same in either style.
fn progress_visual(report: ProgressReport, cx: &App) -> (Hsla, f32) {
    let percent = |default: u8| report.progress.unwrap_or(default) as f32 / 100.0;

    match report.state {
        ProgressState::Set => (cx.theme().primary, percent(0)),
        ProgressState::Error => (cx.theme().danger, percent(100)),
        ProgressState::Pause => (cx.theme().warning, percent(100)),
        // Indeterminate reports carry no percentage: a full-width muted bar
        // reads as "running, no ETA" and stays distinguishable from a finished
        // determinate bar, which is full-width in the accent color. No pulse
        // animation — the strip would then repaint every frame for as long as
        // any background command runs.
        ProgressState::Indeterminate => (cx.theme().muted_foreground, 1.0),
        ProgressState::Remove => (cx.theme().primary, 0.0),
    }
}
