#[cfg(test)]
#[cfg(all(test, enable_profiling))]
#[path = "profiling_tests.rs"]
mod profiling_tests;

#[cfg(enable_profiling)]
use std::time::Duration;

use gpui::App;
#[cfg(enable_profiling)]
use nmt_profiling::transcript::flush;

/// Schedule reporting on the same thread that owns transcript samples.
#[cfg(enable_profiling)]
pub(crate) fn initialize(cx: &mut App) {
    if !nmt_profiling::enabled() {
        return;
    }

    tracing::info!(target: "transcript_perf", "transcript profiling enabled; timings include instrumentation; allocation bytes are traffic, not live memory");

    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;

            cx.update(|_| flush());
        }
    })
    .detach();

    cx.on_app_quit(|_| {
        flush();

        async {}
    })
    .detach();
}

#[cfg(not(enable_profiling))]
pub(crate) fn initialize(_: &mut App) {}
