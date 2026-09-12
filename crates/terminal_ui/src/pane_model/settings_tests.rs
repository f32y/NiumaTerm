use std::time::Duration;

use futures::FutureExt;
use futures::channel::oneshot;
use nmt_config::CursorShape;
use nmt_config::colors::Colors;
use nmt_terminal::session::request::RequestError;
use tokio::time::timeout;

use crate::block_list::chrome::DurationLabels;
use crate::metrics::CellMetrics;
use crate::pane_model::test_session::controller;

#[tokio::test]
async fn settings_refresh_colors_metrics_and_layout_without_repeating_cursor_requests() {
    let (mut model, _) = controller(b"text", false);
    let mut settings = model.settings;

    settings.cursor_shape = CursorShape::Beam;
    settings.fixed_bottom = true;
    settings.pad_rows = 0.0;

    let colors = Colors {
        foreground: [12.0 / 255.0, 34.0 / 255.0, 56.0 / 255.0, 1.0],
        ..Colors::default()
    };

    let labels = DurationLabels {
        seconds: "{seconds} seconds".into(),
        ..DurationLabels::default()
    };

    let update = model
        .update_settings(settings, &colors, labels.clone())
        .unwrap();

    assert_eq!(model.settings.cursor_shape, CursorShape::Beam);
    assert_eq!(model.settings.pad_rows, 0.0);

    let foreground: u32 = model.theme.foreground.into();

    assert_eq!(foreground, 0x0c2238);
    assert_eq!(model.duration_labels.seconds, "{seconds} seconds");
    assert!(model.cell_metrics.is_none());
    assert!(model.frame_cache.needs_rebuild());
    assert!(model.frame_cache.reusable_frame().is_none());
    assert!(model.frame_cache.current().is_some());
    assert!(
        timeout(Duration::from_secs(2), update.failure())
            .await
            .unwrap()
            .is_none()
    );
    assert!(model.update_settings(settings, &colors, labels).is_none());

    let cell = CellMetrics {
        width_px: 10.0,
        height_px: 20.0,
    };

    assert_eq!(model.cell_metrics_or_measure(|| cell), cell);

    model.begin_frame();

    assert_eq!(model.viewport.cursor_y(0, cell.height_px), 100.0);
    assert!(!model.frame_cache.needs_rebuild());
    assert_eq!(
        model.cell_metrics_or_measure(|| panic!("cached metrics must be reused")),
        cell
    );
}

#[test]
fn failed_or_canceled_cursor_requests_restore_only_the_current_requested_shape() {
    for rejected in [false, true] {
        for superseded in [false, true] {
            let (mut model, _) = controller(b"", false);
            let mut settings = model.settings;

            settings.cursor_shape = CursorShape::Beam;

            let mut update = model
                .update_settings(settings, &Colors::default(), DurationLabels::default())
                .unwrap();

            let (reply, request) = oneshot::channel();

            update.request = request;

            if rejected {
                reply.send(Err(RequestError::Unavailable)).unwrap();
            } else {
                drop(reply);
            }

            let failure = update.failure().now_or_never().unwrap().unwrap();

            if superseded {
                settings.cursor_shape = CursorShape::Underline;
                model.update_settings(settings, &Colors::default(), DurationLabels::default());
            }

            model.begin_frame();

            assert_eq!(model.cursor_shape_failed(failure), !superseded);
            assert_eq!(
                model.settings.cursor_shape,
                if superseded {
                    CursorShape::Underline
                } else {
                    CursorShape::Block
                }
            );
            assert_eq!(model.frame_cache.needs_rebuild(), !superseded);
        }
    }
}
