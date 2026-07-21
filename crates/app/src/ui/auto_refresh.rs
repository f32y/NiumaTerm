//! Settings-gated auto-refresh scaffolding shared by the usage widgets.
//!
//! A widget holds a [`RefreshState`] and implements [`AutoRefresh`]; `start`
//! wires the observer/timer lifecycle: refresh on the settings toggle's
//! off→on edge (settings fire for every change, e.g. font size, so the
//! toggle is mirrored to detect the edge), refresh every `INTERVAL` while
//! the toggle is on (the timer loop exits when the entity is dropped), and
//! one initial refresh when the toggle starts on. `refresh` runs a single
//! fetch on the background executor, dropping requests while one is already
//! in flight; errors keep the previous data.

use std::time::Duration;

use gpui::Context;

use crate::ui::AppSettings;

#[derive(Default)]
pub(crate) struct RefreshState {
    pub refreshing: bool,
    /// Mirror of the widget's settings toggle; see the module docs.
    pub enabled: bool,
}

pub(crate) trait AutoRefresh: Sized + 'static {
    type Output: Send + 'static;
    const INTERVAL: Duration;
    fn enabled(settings: &AppSettings) -> bool;
    fn state(&mut self) -> &mut RefreshState;
    fn fetch() -> Self::Output;
    fn apply(&mut self, output: Self::Output);
}

pub(crate) fn start<V: AutoRefresh>(view: &mut V, cx: &mut Context<V>) {
    cx.observe_global::<AppSettings>(|this: &mut V, cx| {
        let enabled = V::enabled(cx.global::<AppSettings>());
        if enabled && !this.state().enabled {
            refresh(this, cx);
        }
        this.state().enabled = enabled;
    })
    .detach();
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(V::INTERVAL).await;
            let alive = this.update(cx, |this, cx| {
                if this.state().enabled {
                    refresh(this, cx);
                }
            });
            if alive.is_err() {
                break;
            }
        }
    })
    .detach();
    if view.state().enabled {
        refresh(view, cx);
    }
}

pub(crate) fn refresh<V: AutoRefresh>(view: &mut V, cx: &mut Context<V>) {
    if view.state().refreshing {
        return;
    }
    view.state().refreshing = true;
    cx.notify();
    let fetch = cx.background_executor().spawn(async move { V::fetch() });
    cx.spawn(async move |this, cx| {
        let output = fetch.await;
        this.update(cx, |this, cx| {
            this.state().refreshing = false;
            this.apply(output);
            cx.notify();
        })
        .ok();
    })
    .detach();
}
