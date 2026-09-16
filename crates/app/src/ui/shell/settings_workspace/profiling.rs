use std::env;
use std::time::{Duration, Instant};

use gpui::{App, Context, InputEvent as _, ScrollDelta, ScrollWheelEvent, Window, point, px, size};
use gpui_component::setting::SelectIndex;
use nmt_profiling::enabled;
use tracing::info;

use crate::agent_updates::AgentUpdates;
use crate::ui::shell::{AppWindow, ShowSettings};

/// Exercise native window drawing only in an explicitly enabled isolated run.
pub(crate) fn start_settings_profile(window: &mut Window, cx: &mut Context<AppWindow>) {
    if !enabled()
        || !cx.global::<AgentUpdates>().testing()
        || env::var_os("NMT_SETTINGS_SCROLL_PROFILE").is_none()
    {
        return;
    }

    cx.spawn_in(window, async move |shell, cx| {
        cx.background_executor().timer(Duration::from_secs(2)).await;

        for (scenario, group, x, width, height) in [
            ("sidebar", 0, 100., 1400., 900.),
            ("themes", 1, 750., 1400., 900.),
            ("themes-compact", 1, 750., 1100., 600.),
        ] {
            if shell.update_in(cx, |shell, window, cx| {
                shell.sidebar.collapsed = true;

                shell.on_show_settings(&ShowSettings, window, cx);

                let open = shell.settings.open.as_mut().expect("settings is open");

                open.state.update(cx, |state, cx| {
                    state.select(SelectIndex { page_ix: 0, group_ix: Some(group) }, cx);
                });

                window.resize(size(px(width), px(height)));

                cx.notify();
            }).is_err() { return; }

            cx.background_executor().timer(Duration::from_secs(2)).await;

            if shell.update_in(cx, |_, window, cx| {
                info!(target: "settings_scroll_profile", scenario, "starting native scrolling sample");

                let started = Instant::now();

                scroll_frame(started, x, window, cx);

                cx.notify();
            }).is_err() { return; }

            cx.background_executor().timer(Duration::from_secs(9)).await;
        }

        let _ = cx.update(|_, cx| {
            info!(target: "settings_scroll_profile", "native scrolling samples complete");

            cx.quit();
        });
    }).detach();
}

fn scroll_frame(started: Instant, x: f32, window: &mut Window, cx: &mut App) {
    let elapsed = started.elapsed();

    if elapsed >= Duration::from_secs(8) {
        return;
    }

    let direction = if elapsed.as_millis() / 750 % 2 == 0 {
        -1.
    } else {
        1.
    };

    window.dispatch_event(
        ScrollWheelEvent {
            position: point(px(x), window.viewport_size().height / 2.),
            delta: ScrollDelta::Pixels(point(px(0.), px(direction * 12.))),
            ..Default::default()
        }
        .to_platform_input(),
        cx,
    );

    window.on_next_frame(move |window, cx| scroll_frame(started, x, window, cx));
}
